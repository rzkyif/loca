// Pins the unified-installer templates (pure string builders; the real
// makensis/productbuild runs only happen on provisioned release runners):
// the silent-flag surface, the section/choice wiring, the ARP-entry ownership
// handoff, and the option-marker mechanism the macOS pkg uses.

import { assertEquals, assertMatch, assertStringIncludes } from "@std/assert";

import {
  coreDisplayName,
  macPkgIds,
  macSetupPostinstall,
  renderMacUninstall,
  unifiedDisplayName,
  unifiedDistributionXml,
  unifiedInstallerStableName,
  unifiedNsi,
} from "./unified-installers.ts";

function nsi(overrides: Partial<Parameters<typeof unifiedNsi>[0]> = {}): string {
  return unifiedNsi({
    channel: "latest",
    coreVersion: "0.1.5",
    clientVersion: "0.1.13",
    payloadDir: "C:\\work\\payload",
    clientExePath: "C:\\work\\client-setup.exe",
    binName: "tomat-core-latest.exe",
    outFile: "C:\\work\\tomat-latest-setup-0.1.13.exe",
    hasExtensions: false,
    ...overrides,
  });
}

Deno.test("names: stable vs channel-suffixed", () => {
  assertEquals(unifiedInstallerStableName("exe", "stable"), "tomat-setup.exe");
  assertEquals(unifiedInstallerStableName("exe", "latest"), "tomat-latest-setup.exe");
  assertEquals(unifiedInstallerStableName("pkg", "stable"), "tomat.pkg");
  assertEquals(unifiedInstallerStableName("pkg", "latest"), "tomat-latest.pkg");
  assertEquals(unifiedDisplayName("stable"), "Tomat");
  assertEquals(unifiedDisplayName("latest"), "Tomat (latest)");
  assertEquals(coreDisplayName("latest"), "Tomat Core (latest)");
});

Deno.test("nsi: silent flag surface is parsed in .onInit", () => {
  const src = nsi();
  for (const flag of ["/CLIENT=", "/CORE=", "/SERVICE=", "/BINDALL=", "/BEHINDPROXY="]) {
    assertStringIncludes(src, `\${GetOptions} $R0 "${flag}"`);
  }
  // /CLIENT=0 and /CORE=0 unselect their sections (core-only / client-only
  // headless installs).
  assertStringIncludes(src, "!insertmacro UnselectSection ${SEC_CLIENT}");
  assertStringIncludes(src, "!insertmacro UnselectSection ${SEC_CORE}");
});

Deno.test("nsi: client is a required section wrapping the embedded Tauri setup", () => {
  const src = nsi();
  assertStringIncludes(src, 'Section "Tomat Client (required)" SEC_CLIENT');
  // Read-only in the GUI (the required half); /CLIENT=0 still unselects it.
  assertMatch(src, /SEC_CLIENT\n(\s*;[^\n]*\n)*\s*SectionIn RO/);
  assertStringIncludes(src, `ExecWait '"$PLUGINSDIR\\client-setup.exe" /S'`);
  // Per-user, no admin (matches the Tauri NSIS installMode currentUser).
  assertStringIncludes(src, "RequestExecutionLevel user");
});

Deno.test("nsi: core section provisions via install-service and owns the ARP entry", () => {
  const src = nsi();
  assertStringIncludes(src, 'Section "Tomat Core" SEC_CORE');
  // The umbrella's uninstall.exe owns Add/Remove; install-service must not
  // clobber it back to the PowerShell teardown.
  assertStringIncludes(src, "set TOMAT_INSTALL_REGISTER_ARP=0&&");
  assertStringIncludes(src, 'WriteUninstaller "$INSTDIR\\uninstall.exe"');
  assertStringIncludes(src, 'DisplayName" "Tomat Core (latest)"');
  // Background mode avoids nsExec's inherited-pipe hang; service mode keeps the
  // hidden console.
  assertMatch(src, /\$\{If\} \$ServiceFlag == "0"\s*\n\s*ExecWait/);
  assertMatch(src, /\$\{Else\}\s*\n\s*nsExec::Exec/);
});

Deno.test("nsi: uninstaller takes /KEEPDATA + /PURGEMODELS and forwards them", () => {
  const src = nsi();
  assertStringIncludes(src, `\${GetOptions} $R0 "/KEEPDATA"`);
  assertStringIncludes(src, `\${GetOptions} $R0 "/PURGEMODELS"`);
  assertStringIncludes(src, 'StrCpy $R9 "$R9 --keep-data"');
  assertStringIncludes(src, 'StrCpy $R9 "$R9 --purge-models"');
  assertStringIncludes(src, "uninstall-service");
});

Deno.test("nsi: keep-data uninstall never sweeps the whole data root", () => {
  const src = nsi();
  const uninstall = src.slice(src.indexOf('Section "Uninstall"'));
  // $INSTDIR is the Core's DATA root; the full-tree sweep must sit in the
  // else branch of the keep-data split, with keep-data removing only what the
  // installer laid down.
  assertMatch(
    uninstall,
    /\$\{If\} \$KeepDataFlag == "1"\s*\n\s*RMDir \/r "\$INSTDIR\\bin"[\s\S]*?\$\{Else\}\s*\n\s*RMDir \/r "\$INSTDIR"\s*\n\s*\$\{EndIf\}/,
  );
  assertStringIncludes(uninstall, 'RMDir /r "$INSTDIR\\workers"');
  assertStringIncludes(uninstall, 'Delete "$INSTDIR\\uninstall.exe"');
  // Exactly one unconditional-looking full sweep, and it is the else branch.
  assertEquals(uninstall.split('RMDir /r "$INSTDIR"\n').length, 2);
});

Deno.test("nsi: extensions payload block only when staged", () => {
  // The uninstaller's keep-data sweep always names the planted seed files; the
  // INSTALL payload block is what the staging flag gates.
  assertEquals(nsi().includes('SetOutPath "$INSTDIR\\extensions"'), false);
  assertStringIncludes(nsi({ hasExtensions: true }), 'SetOutPath "$INSTDIR\\extensions"');
});

Deno.test("distribution: choice tree, required client, hidden setup last", () => {
  const xml = unifiedDistributionXml({
    channel: "latest",
    coreVersion: "0.1.5",
    clientVersion: "0.1.13",
  });
  const ids = macPkgIds("latest");
  assertEquals(ids.client, "au.tomat.client-latest");
  assertEquals(ids.setup, "au.tomat.core-latest.setup");
  // Client is preselected + disabled (required in the GUI; headless core-only
  // re-enables then deselects it via choices XML).
  assertMatch(xml, /<choice id="client"[^>]*start_selected="true" start_enabled="false"/);
  // Core is preselected and deselectable; option markers follow the core choice.
  assertMatch(xml, /<choice id="core"[^>]*start_selected="true"/);
  assertMatch(xml, /<choice id="core-service"[^>]*start_selected="true"/);
  assertMatch(xml, /<choice id="core-lan"[^>]*start_selected="false"/);
  // behind-proxy is a hidden, headless-only choice.
  assertMatch(xml, /<choice id="core-behindproxy"[^>]*visible="false"/);
  // The setup component is hidden, tied to the core choice, and LAST in the
  // outline so its postinstall runs after every marker is on disk.
  assertMatch(
    xml,
    /<choice id="core-setup"[^>]*visible="false"[^>]*selected="choices.core.selected"/,
  );
  const outline = xml.slice(xml.indexOf("<choices-outline>"), xml.indexOf("</choices-outline>"));
  const lines = [...outline.matchAll(/<line choice="([^"]+)"/g)].map((m) => m[1]);
  assertEquals(lines.at(-1), "core-setup");
  // Every choice's pkg-ref resolves to a component pkg.
  for (const id of Object.values(ids)) {
    assertMatch(xml, new RegExp(`<pkg-ref id="${id.replaceAll(".", "\\.")}" version="[^"]+"`));
  }
});

Deno.test("mac setup postinstall reads markers, provisions as the console user", () => {
  const script = macSetupPostinstall("latest", "/usr/local/lib/tomat-latest", "tomat-core-latest");
  // Marker files -> TOMAT_INSTALL_* env; markers removed after reading.
  assertStringIncludes(script, 'if [ -f "$PREFIX/opts/service" ]; then SERVICE=1; fi');
  assertStringIncludes(script, 'if [ -f "$PREFIX/opts/lan" ]; then BINDALL=1; fi');
  assertStringIncludes(script, 'if [ -f "$PREFIX/opts/behindproxy" ]; then BEHINDPROXY=1; fi');
  assertStringIncludes(script, 'rm -rf "$PREFIX/opts"');
  // Core deselected -> no payload -> nothing provisioned.
  assertStringIncludes(script, 'if [ ! -d "$PREFIX/bin" ]; then');
  // install-service runs AS the console user with HOME pinned.
  assertStringIncludes(script, 'sudo -u "$CONSOLE_USER" env HOME="$USER_HOME"');
  assertStringIncludes(script, 'TOMAT_INSTALL_SERVICE="$SERVICE"');
  assertStringIncludes(script, "install-service");
});

Deno.test("mac uninstall template renders every token", () => {
  const template = [
    'CHANNEL="__CHANNEL__"',
    'PREFIX="__PREFIX__"',
    'APP_NAME="__APP_NAME__"',
    'BIN_NAME="__BIN_NAME__"',
    'PKG_IDS="__PKG_IDS__"',
    'SERVICE_LABEL="__SERVICE_LABEL__"',
  ].join("\n");
  const out = renderMacUninstall(template, {
    channel: "latest",
    prefix: "/usr/local/lib/tomat-latest",
    appName: "Tomat (latest).app",
    binName: "tomat-core-latest",
    pkgIds: Object.values(macPkgIds("latest")),
    serviceLabel: macPkgIds("latest").core,
  });
  assertEquals(out.includes("__"), false);
  assertStringIncludes(out, 'CHANNEL="latest"');
  assertStringIncludes(out, 'APP_NAME="Tomat (latest).app"');
  assertStringIncludes(out, "au.tomat.core-latest.setup");
  assertStringIncludes(out, 'SERVICE_LABEL="au.tomat.core-latest"');
});

Deno.test("the tracked mac uninstaller template carries every token the renderer fills", async () => {
  const template = await Deno.readTextFile(
    new URL("./templates/mac-uninstall.sh", import.meta.url),
  );
  for (const token of [
    "__CHANNEL__",
    "__PREFIX__",
    "__APP_NAME__",
    "__BIN_NAME__",
    "__PKG_IDS__",
    "__SERVICE_LABEL__",
  ]) {
    assertStringIncludes(template, token);
  }
  const out = renderMacUninstall(template, {
    channel: "stable",
    prefix: "/usr/local/lib/tomat-stable",
    appName: "tomat.app",
    binName: "tomat-core",
    pkgIds: Object.values(macPkgIds("stable")),
    serviceLabel: macPkgIds("stable").core,
  });
  assertEquals(out.includes("__"), false);
  // The flag surface the manual documents.
  for (const flag of ["--keep-data", "--purge-models", "--keep-client"]) {
    assertStringIncludes(out, flag);
  }
});
