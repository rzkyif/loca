// The unified native installers: ONE user-facing download per desktop platform
// (Windows NSIS .exe, macOS distribution .pkg) that installs the Client
// (required) and optionally the Core (checked by default) with the Core's
// install options, replacing the separate client and core downloads. Linux
// keeps its split (client AppImage + the core .deb/.rpm from
// core-installers.ts), the one platform without a unified artifact.
//
// Both installers WRAP the Tauri-built client artifact - the Windows umbrella
// embeds the Tauri per-user NSIS setup and runs it silently; the macOS pkg
// wraps the signed .app as a component package - so Tauri keeps producing the
// self-update artifacts unchanged and each half keeps its own uninstall entry.
// The Core half stages the payload assembled by core-installers.ts and
// delegates provisioning to the binary's own `install-service`.
//
// Headless surface (documented in the manual's installing article):
//   Windows: loca-setup.exe /S [/CLIENT=0|1] [/CORE=0|1] [/SERVICE=0|1]
//            [/BINDALL=0|1] [/BEHINDPROXY=0|1]
//   macOS:   sudo installer -pkg loca.pkg -target /
//            [-applyChoiceChangesXML choices.xml]
//   Uninstall (Windows Core entry): uninstall.exe /S [/KEEPDATA] [/PURGEMODELS]
//   Uninstall (macOS): sudo <prefix>/uninstall.sh [--keep-data]
//            [--purge-models] [--keep-client]
//
// VALIDATION NOTE: like core-installers.ts, this module drives makensis /
// pkgbuild / productbuild / productsign and only runs on a provisioned release
// runner; the templates are exported pure and pinned by unified-installers.test.ts.

import { basename, join } from "@std/path";
import { ensureDir } from "@std/fs/ensure-dir";
import type { Triple } from "../../packages/loca-shared/src/domain/model.ts";
import { BUILTIN_EXTENSION_ID } from "../../packages/loca-shared/src/domain/extension.ts";
import type { BuildArtifact, CoreBuildArtifacts, HelperArtifact, WorkerArtifact } from "./core.ts";
import {
  assemblePayload,
  have,
  type InstallerAsset,
  notarizePkg,
  record,
  run,
  signWindows,
} from "./core-installers.ts";
import { materializeInstallerKeychain } from "./apple-toolchain.ts";
import {
  channelBinSuffix,
  channelManifestDir,
  colors,
  type DeployEnv,
  DIST_DIR,
  fail,
  fetchLiveJson,
  info,
  type ReleaseChannel,
  REPO_ROOT,
  sha256File,
} from "./lib.ts";

export interface UnifiedInstallerOpts {
  coreVersion: string;
  clientVersion: string;
  channel: ReleaseChannel;
  triple: Triple;
  env: DeployEnv;
}

/** Version-less download filename for a unified installer, matching the
 *  installerAliasName() half in core-installers.ts and appInstallers() in the
 *  website's lib/install.ts. */
export function unifiedInstallerStableName(format: "exe" | "pkg", channel: ReleaseChannel): string {
  const suffix = channelBinSuffix(channel);
  return format === "exe" ? `loca${suffix}-setup.exe` : `loca${suffix}.pkg`;
}

/** The human display names, matching the Client's parenthesized channel
 *  convention ("Loca (latest)"): never the raw -latest suffix. */
export function unifiedDisplayName(channel: ReleaseChannel): string {
  return channel === "stable" ? "Loca" : `Loca (${channel})`;
}
export function coreDisplayName(channel: ReleaseChannel): string {
  return channel === "stable" ? "Loca Core" : `Loca Core (${channel})`;
}

/** The neutral system prefix the macOS pkg stages the Core payload under; the
 *  setup postinstall copies it into the console user's ~/.loca/<channel>/core. */
function macInstallPrefix(channel: ReleaseChannel): string {
  const suffix = channelBinSuffix(channel);
  return `/usr/local/lib/loca${suffix || "-stable"}`;
}

// ---------------------------------------------------------------------------
// offline built-in extension plant (best effort)

/** Stage the live published built-in extension into `<root>/extensions/` as the
 *  planted `.loca-extension-builtin.{tgz,json}` pair, so install-service's
 *  bootstrap finds them on disk and skips its install-time fetch (the install
 *  lands fully offline). Best effort: any failure just means bootstrap fetches
 *  as usual, and core re-verifies the Ed25519 signature offline either way. The
 *  manifest is planted byte-for-byte (the signature covers the exact bytes). */
export async function stageBuiltinExtension(
  root: string,
  env: DeployEnv,
  channel: ReleaseChannel,
): Promise<boolean> {
  try {
    const url = `https://${env.storageDomain}/${channelManifestDir(channel)}/extension.json`;
    const res = await fetch(url);
    if (!res.ok) {
      info(
        colors.yellow(
          `built-in extension manifest unavailable (HTTP ${res.status}); ` +
            `the installer payload ships without it (install-service fetches it instead)`,
        ),
      );
      return false;
    }
    const raw = await res.text();
    const manifest = JSON.parse(raw) as { tarballUrl?: string; sha256?: string };
    if (!manifest.tarballUrl || !manifest.sha256) return false;
    const extDir = join(root, "extensions");
    await ensureDir(extDir);
    const tarballPath = join(extDir, `.${BUILTIN_EXTENSION_ID}.tgz`);
    const tarRes = await fetch(manifest.tarballUrl);
    if (!tarRes.ok || !tarRes.body) return false;
    const file = await Deno.open(tarballPath, { create: true, write: true, truncate: true });
    await tarRes.body.pipeTo(file.writable);
    const { sha256 } = await sha256File(tarballPath);
    if (sha256 !== manifest.sha256.toLowerCase()) {
      await Deno.remove(tarballPath).catch(() => {});
      return false;
    }
    await Deno.writeTextFile(join(extDir, `.${BUILTIN_EXTENSION_ID}.json`), raw);
    return true;
  } catch (err) {
    info(
      colors.yellow(
        `built-in extension plant skipped for the installer payload: ` +
          `${err instanceof Error ? err.message : err}`,
      ),
    );
    return false;
  }
}

// ---------------------------------------------------------------------------
// core payload from the live core.json (client-only local releases)

interface LiveCoreManifest {
  version: string;
  binaries: Array<{ triple: Triple; url: string; sha256: string }>;
  workers: Array<{ name: string; url: string; sha256: string }>;
  helpers: Array<{ name: string; triple: Triple; url: string; sha256: string }>;
}

/** Reconstruct the host triple's CoreBuildArtifacts by downloading + verifying
 *  the published artifacts named in the live core.json, for the local release
 *  path where the client item (which owns the unified installers) runs without
 *  a fresh core build. URLs point at gzip'd files; sha256 is over the
 *  DECOMPRESSED bytes, exactly like self-install verifies. */
export async function materializeCorePayload(
  env: DeployEnv,
  channel: ReleaseChannel,
  triple: Triple,
): Promise<{ version: string; built: CoreBuildArtifacts }> {
  // Cache-bust the manifest read: core.json is served with a short max-age and
  // this often runs minutes after the SAME release run published a new core, so
  // a cached edge read would wrap the previous core and leave installers.json
  // desynced from core.json (the artifact URLs below are version-namespaced and
  // immutable, so only the manifest needs the buster).
  const manifest = await fetchLiveJson<LiveCoreManifest>(
    env,
    `${channelManifestDir(channel)}/core.json?fresh=${Date.now()}`,
  );
  if (!manifest) fail(`no live core.json for ${channel}; release the core first`);
  const work = await Deno.makeTempDir({ prefix: "loca-core-payload-" });

  const download = async (url: string, sha256: string, exec = false): Promise<string> => {
    const name = basename(new URL(url).pathname).replace(/\.gz$/, "");
    const dest = join(work, name);
    // Fast path: a combined core+client release leaves this exact artifact
    // uncompressed under dist/<triple>/ from the core build moments earlier;
    // reuse it instead of re-downloading. Same trust either way: the artifact
    // must hash to the manifest's sha256 below, so a stale dist file simply
    // fails the match and falls back to the download.
    let staged = false;
    try {
      const local = join(DIST_DIR, triple, name);
      if ((await sha256File(local)).sha256 === sha256) {
        await Deno.copyFile(local, dest);
        staged = true;
      }
    } catch {
      /* not on disk locally */
    }
    if (!staged) {
      const res = await fetch(url);
      if (!res.ok || !res.body) fail(`fetch ${url} returned ${res.status}`);
      const gunzip = res.body.pipeThrough(new DecompressionStream("gzip"));
      const file = await Deno.open(dest, { create: true, write: true, truncate: true });
      await gunzip.pipeTo(file.writable);
    }
    const got = await sha256File(dest);
    if (got.sha256 !== sha256) {
      fail(`sha256 mismatch for ${url}: expected ${sha256}, got ${got.sha256}`);
    }
    // A fresh download lands 0644; the pkg/deb/rpm staging preserves modes, so
    // the executables must get their bit here or install-service fails with
    // "permission denied" on mac/linux (mirrors the self-updater's chmod).
    if (exec && Deno.build.os !== "windows") await Deno.chmod(dest, 0o755);
    return dest;
  };

  const bin = manifest.binaries.find((b) => b.triple === triple);
  if (!bin) fail(`live core.json has no binary for ${triple}`);
  const binPath = await download(bin.url, bin.sha256, true);
  const artifacts: BuildArtifact[] = [
    {
      triple,
      name: "loca-core",
      filename: basename(binPath),
      path: binPath,
      sha256: bin.sha256,
      size: (await Deno.stat(binPath)).size,
    },
  ];
  const helpers: HelperArtifact[] = [];
  for (const h of manifest.helpers.filter((h) => h.triple === triple)) {
    const path = await download(h.url, h.sha256, true);
    helpers.push({
      triple,
      entryName: h.name,
      filename: basename(path),
      path,
      sha256: h.sha256,
      size: (await Deno.stat(path)).size,
    });
  }
  const workers: WorkerArtifact[] = [];
  for (const w of manifest.workers) {
    const path = await download(w.url, w.sha256);
    workers.push({ name: w.name, path, sha256: w.sha256, size: (await Deno.stat(path)).size });
  }
  return { version: manifest.version, built: { artifacts, helpers, workers } };
}

// ---------------------------------------------------------------------------
// Windows umbrella NSIS .exe

/** Build the unified Windows installer: the umbrella NSIS .exe embedding the
 *  Tauri client setup (run silently) + the Core payload. Returns [] when
 *  makensis is unavailable so a plain host build degrades rather than fails. */
export async function buildWindowsUnified(
  built: CoreBuildArtifacts,
  clientExeDistRelPath: string,
  opts: UnifiedInstallerOpts,
): Promise<InstallerAsset[]> {
  if (!(await have("makensis"))) {
    info(
      colors.yellow(`makensis not available; skipping the unified installer for ${opts.triple}`),
    );
    return [];
  }
  const { channel, triple, env, clientVersion } = opts;
  const suffix = channelBinSuffix(channel);
  const work = await Deno.makeTempDir({ prefix: "loca-unified-nsis-" });
  const payload = join(work, "payload");
  const { binName } = await assemblePayload(built, triple, channel, payload);
  const hasExtensions = await stageBuiltinExtension(payload, env, channel);
  // A fixed embedded name so the template never depends on Tauri's
  // space-and-version bundle filename.
  const clientExe = join(work, "client-setup.exe");
  await Deno.copyFile(join(DIST_DIR, clientExeDistRelPath), clientExe);

  const nsi = join(work, "loca.nsi");
  const outFile = join(work, `loca${suffix}-setup-${clientVersion}.exe`);
  await Deno.writeTextFile(
    nsi,
    unifiedNsi({
      channel,
      coreVersion: opts.coreVersion,
      clientVersion,
      payloadDir: payload,
      clientExePath: clientExe,
      binName,
      outFile,
      hasExtensions,
    }),
  );
  await run(["makensis", nsi]);

  if (env.windowsCertificateThumbprint || env.windowsSignCommand) {
    await signWindows(outFile, env);
  } else {
    info(colors.yellow(`no Windows cert; shipping an unsigned installer`));
  }
  return [await record(outFile, triple, "unified", "exe", clientVersion)];
}

export interface UnifiedNsiOpts {
  channel: ReleaseChannel;
  coreVersion: string;
  clientVersion: string;
  payloadDir: string;
  clientExePath: string;
  binName: string;
  outFile: string;
  hasExtensions: boolean;
}

/** The umbrella .nsi source. Exported pure for unified-installers.test.ts.
 *
 *  Per-user (no admin). GUI flow: components page (Client read-only + Core,
 *  both checked) then the Core options page (only when Core is selected), then
 *  install. Silent flow: /S skips every page; /CLIENT= /CORE= /SERVICE=
 *  /BINDALL= /BEHINDPROXY= parsed in .onInit set the same state the pages
 *  would (core-only headless install: /S /CLIENT=0).
 *
 *  The Client section runs the embedded Tauri per-user setup silently; the
 *  Client keeps its own Add/Remove entry + uninstaller, and the in-app updater
 *  keeps updating it. The Core section lays the payload down and provisions
 *  via the binary's own install-service, with LOCA_INSTALL_REGISTER_ARP=0:
 *  THIS installer's uninstall.exe owns the Core's Add/Remove entry (written
 *  below), and the skip keeps install-service re-runs from clobbering it.
 *
 *  The uninstaller (the Core's Add/Remove entry; the Client has its own) shows
 *  a keep-my-data / remove-models options page, or takes /KEEPDATA and
 *  /PURGEMODELS when silent, and hands both to `uninstall-service`. */
export function unifiedNsi(o: UnifiedNsiOpts): string {
  const { channel, binName } = o;
  const suffix = channelBinSuffix(channel);
  const displayName = unifiedDisplayName(channel);
  const coreName = coreDisplayName(channel);
  const extensionsBlock = o.hasExtensions
    ? `  SetOutPath "$INSTDIR\\extensions"
  File /r "${join(o.payloadDir, "extensions")}\\*"
`
    : "";
  return `!include "MUI2.nsh"
!include "nsDialogs.nsh"
!include "LogicLib.nsh"
!include "Sections.nsh"
!include "FileFunc.nsh"
Name "${displayName}"
OutFile "${o.outFile}"
RequestExecutionLevel user
InstallDir "$PROFILE\\.loca\\${channel}\\core"
!define UNINST_KEY "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\loca-core${suffix}"

Var ServiceCheckbox
Var BindAllCheckbox
Var ServiceFlag
Var BindAllFlag
Var BehindProxyFlag
Var KeepDataCheckbox
Var PurgeModelsCheckbox
Var KeepDataFlag
Var PurgeModelsFlag

!insertmacro MUI_PAGE_COMPONENTS
Page custom CoreOptionsPage CoreOptionsPageLeave
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_TITLE "${displayName} is installed"
!define MUI_FINISHPAGE_TEXT "Open Loca from the Start Menu to get started."
!insertmacro MUI_PAGE_FINISH
UninstPage custom un.OptionsPage un.OptionsPageLeave
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Loca Client (required)" SEC_CLIENT
  ; Read-only on the components page: the Client is the required half in the
  ; GUI. RO only locks the checkbox; the silent /CLIENT=0 UnselectSection in
  ; .onInit still works for a core-only headless install.
  SectionIn RO
  ; The embedded Tauri per-user setup installs, registers, and updates the
  ; Client on its own; running it silently keeps this installer's UI the one
  ; surface the user sees.
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "${o.clientExePath}"
  ExecWait '"$PLUGINSDIR\\client-setup.exe" /S' $0
  DetailPrint "client setup exited $0"
SectionEnd

Section "Loca Core" SEC_CORE
  SetOutPath "$INSTDIR\\bin"
  File /r "${join(o.payloadDir, "bin")}\\*"
  SetOutPath "$INSTDIR\\workers"
  File /r "${join(o.payloadDir, "workers")}\\*"
${extensionsBlock}  WriteUninstaller "$INSTDIR\\uninstall.exe"
  WriteRegStr HKCU "\${UNINST_KEY}" "DisplayName" "${coreName}"
  WriteRegStr HKCU "\${UNINST_KEY}" "DisplayVersion" "${o.coreVersion}"
  WriteRegStr HKCU "\${UNINST_KEY}" "Publisher" "Loca"
  WriteRegStr HKCU "\${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "\${UNINST_KEY}" "UninstallString" '"$INSTDIR\\uninstall.exe"'
  WriteRegStr HKCU "\${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\\uninstall.exe" /S'
  WriteRegDWORD HKCU "\${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "\${UNINST_KEY}" "NoRepair" 1
  ; Provision + start via the binary's own subcommand (bootstrap, service or
  ; background launch). LOCA_INSTALL_REGISTER_ARP=0: the entry written above
  ; owns Add/Remove, see the module comment.
  ; Background mode must NOT run under nsExec: nsExec reads the command's
  ; output through a pipe, the detached core that install-service leaves
  ; running inherits that pipe's write end (every spawn hop on Windows passes
  ; bInheritHandles), and nsExec's read-to-EOF then blocks forever even after
  ; install-service exits. ExecWait uses no pipe, at the cost of a briefly
  ; visible console. Service mode keeps nsExec's hidden console: the Task
  ; Scheduler starts the core out-of-tree, so no long-lived process ever holds
  ; the pipe.
  \${If} $ServiceFlag == "0"
    ExecWait 'cmd.exe /c set LOCA_CHANNEL=${channel}&& set LOCA_INSTALL_SERVICE=$ServiceFlag&& set LOCA_INSTALL_BIND_ALL=$BindAllFlag&& set LOCA_INSTALL_BEHIND_PROXY=$BehindProxyFlag&& set LOCA_INSTALL_REGISTER_ARP=0&& "$INSTDIR\\bin\\${binName}" install-service' $0
  \${Else}
    nsExec::Exec 'cmd.exe /c set LOCA_CHANNEL=${channel}&& set LOCA_INSTALL_SERVICE=$ServiceFlag&& set LOCA_INSTALL_BIND_ALL=$BindAllFlag&& set LOCA_INSTALL_BEHIND_PROXY=$BehindProxyFlag&& set LOCA_INSTALL_REGISTER_ARP=0&& "$INSTDIR\\bin\\${binName}" install-service'
    Pop $0
  \${EndIf}
  DetailPrint "install-service exited $0"
SectionEnd

Function .onInit
  StrCpy $ServiceFlag "1"
  StrCpy $BindAllFlag "0"
  StrCpy $BehindProxyFlag "0"
  \${GetParameters} $R0
  ClearErrors
  \${GetOptions} $R0 "/CLIENT=" $R1
  \${IfNot} \${Errors}
    \${If} $R1 == "0"
      !insertmacro UnselectSection \${SEC_CLIENT}
    \${EndIf}
  \${EndIf}
  ClearErrors
  \${GetOptions} $R0 "/CORE=" $R1
  \${IfNot} \${Errors}
    \${If} $R1 == "0"
      !insertmacro UnselectSection \${SEC_CORE}
    \${EndIf}
  \${EndIf}
  ClearErrors
  \${GetOptions} $R0 "/SERVICE=" $R1
  \${IfNot} \${Errors}
    StrCpy $ServiceFlag $R1
  \${EndIf}
  ClearErrors
  \${GetOptions} $R0 "/BINDALL=" $R1
  \${IfNot} \${Errors}
    StrCpy $BindAllFlag $R1
  \${EndIf}
  ClearErrors
  \${GetOptions} $R0 "/BEHINDPROXY=" $R1
  \${IfNot} \${Errors}
    StrCpy $BehindProxyFlag $R1
  \${EndIf}
FunctionEnd

Function CoreOptionsPage
  \${IfNot} \${SectionIsSelected} \${SEC_CORE}
    Abort
  \${EndIf}
  !insertmacro MUI_HEADER_TEXT "Core Options" "Choose how the Core runs on this computer."
  nsDialogs::Create 1018
  Pop $0
  \${If} $0 == error
    Abort
  \${EndIf}
  \${NSD_CreateCheckbox} 0 10u 100% 12u "Keep the Core running in the background"
  Pop $ServiceCheckbox
  \${NSD_Check} $ServiceCheckbox
  \${NSD_CreateLabel} 10u 24u -10u 20u "Starts the Core at login so it is always ready. When off, the Client starts it on demand."
  Pop $0
  \${NSD_CreateCheckbox} 0 52u 100% 12u "Allow other devices on this network to pair"
  Pop $BindAllCheckbox
  \${NSD_CreateLabel} 10u 66u -10u 20u "When off, only this computer can reach the Core. Changeable later in the Core's settings.json."
  Pop $0
  nsDialogs::Show
FunctionEnd

Function CoreOptionsPageLeave
  \${NSD_GetState} $ServiceCheckbox $0
  \${If} $0 == \${BST_CHECKED}
    StrCpy $ServiceFlag "1"
  \${Else}
    StrCpy $ServiceFlag "0"
  \${EndIf}
  \${NSD_GetState} $BindAllCheckbox $0
  \${If} $0 == \${BST_CHECKED}
    StrCpy $BindAllFlag "1"
  \${Else}
    StrCpy $BindAllFlag "0"
  \${EndIf}
FunctionEnd

Function un.onInit
  StrCpy $KeepDataFlag "0"
  StrCpy $PurgeModelsFlag "0"
  \${GetParameters} $R0
  ClearErrors
  \${GetOptions} $R0 "/KEEPDATA" $R1
  \${IfNot} \${Errors}
    StrCpy $KeepDataFlag "1"
  \${EndIf}
  ClearErrors
  \${GetOptions} $R0 "/PURGEMODELS" $R1
  \${IfNot} \${Errors}
    StrCpy $PurgeModelsFlag "1"
  \${EndIf}
FunctionEnd

Function un.OptionsPage
  !insertmacro MUI_HEADER_TEXT "Remove ${coreName}" "Choose what stays on this computer."
  nsDialogs::Create 1018
  Pop $0
  \${If} $0 == error
    Abort
  \${EndIf}
  \${NSD_CreateCheckbox} 0 10u 100% 12u "Keep my settings and data"
  Pop $KeepDataCheckbox
  \${NSD_CreateLabel} 10u 24u -10u 20u "Sessions, memories, and settings under $PROFILE\\.loca\\${channel}\\core."
  Pop $0
  \${NSD_CreateCheckbox} 0 52u 100% 12u "Also remove downloaded models"
  Pop $PurgeModelsCheckbox
  \${NSD_CreateLabel} 10u 66u -10u 20u "Models under $PROFILE\\.loca\\models are shared across channels and kept by default."
  Pop $0
  nsDialogs::Show
FunctionEnd

Function un.OptionsPageLeave
  \${NSD_GetState} $KeepDataCheckbox $0
  \${If} $0 == \${BST_CHECKED}
    StrCpy $KeepDataFlag "1"
  \${Else}
    StrCpy $KeepDataFlag "0"
  \${EndIf}
  \${NSD_GetState} $PurgeModelsCheckbox $0
  \${If} $0 == \${BST_CHECKED}
    StrCpy $PurgeModelsFlag "1"
  \${Else}
    StrCpy $PurgeModelsFlag "0"
  \${EndIf}
FunctionEnd

Section "Uninstall"
  StrCpy $R9 "uninstall-service"
  \${If} $KeepDataFlag == "1"
    StrCpy $R9 "$R9 --keep-data"
  \${EndIf}
  \${If} $PurgeModelsFlag == "1"
    StrCpy $R9 "$R9 --purge-models"
  \${EndIf}
  nsExec::Exec 'cmd.exe /c set LOCA_CHANNEL=${channel}&& "$INSTDIR\\bin\\${binName}" $R9'
  Pop $0
  DeleteRegKey HKCU "\${UNINST_KEY}"
  ; $INSTDIR doubles as the Core's DATA root (settings, sessions, memories), so
  ; keep-data sweeps only what this installer laid down: the binaries, the
  ; planted built-in extension seed, and the uninstaller itself. The bare RMDir
  ; only removes the dirs when nothing else is left in them.
  \${If} $KeepDataFlag == "1"
    RMDir /r "$INSTDIR\\bin"
    RMDir /r "$INSTDIR\\workers"
    Delete "$INSTDIR\\extensions\\.${BUILTIN_EXTENSION_ID}.*"
    RMDir "$INSTDIR\\extensions"
    Delete "$INSTDIR\\uninstall.exe"
    RMDir "$INSTDIR"
  \${Else}
    RMDir /r "$INSTDIR"
  \${EndIf}
SectionEnd
`;
}

// ---------------------------------------------------------------------------
// macOS unified distribution .pkg

const MAC_UNINSTALL_TEMPLATE = join(REPO_ROOT, "scripts/release/templates/mac-uninstall.sh");

/** Component-package ids inside the unified pkg. Exported for tests. */
export function macPkgIds(channel: ReleaseChannel): {
  client: string;
  core: string;
  optService: string;
  optLan: string;
  optBehindProxy: string;
  setup: string;
} {
  const suffix = channelBinSuffix(channel);
  const core = `com.rzkyif.loca.core${suffix}`;
  return {
    client: `com.rzkyif.loca.client${suffix}`,
    core,
    optService: `${core}.opt-service`,
    optLan: `${core}.opt-lan`,
    optBehindProxy: `${core}.opt-behindproxy`,
    setup: `${core}.setup`,
  };
}

/** Locate the Tauri-built .app. A per-triple `--target` build and a host-native
 *  one bundle into different dirs, and a stale .app can linger in whichever dir
 *  this run did NOT write, so when both exist pick the most recently built (by
 *  its Info.plist, which every bundle pass rewrites). */
async function findMacApp(triple: Triple): Promise<string> {
  const targetDir = Deno.env.get("CARGO_TARGET_DIR") ?? join(REPO_ROOT, "target");
  const candidates = [
    join(targetDir, triple, "release", "bundle", "macos"),
    join(targetDir, "release", "bundle", "macos"),
  ];
  let best: { path: string; mtime: number } | null = null;
  for (const dir of candidates) {
    try {
      for await (const entry of Deno.readDir(dir)) {
        if (!entry.isDirectory || !entry.name.endsWith(".app")) continue;
        const path = join(dir, entry.name);
        const stamp = await Deno.stat(join(path, "Contents", "Info.plist")).catch(() =>
          Deno.stat(path),
        );
        const mtime = stamp.mtime?.getTime() ?? 0;
        if (!best || mtime > best.mtime) best = { path, mtime };
      }
    } catch {
      // candidate dir absent
    }
  }
  if (best) return best.path;
  fail(
    `no .app under ${candidates.join(" or ")}; the unified pkg must be built right after ` +
      `the client build, while the Tauri bundle dir is on disk`,
  );
}

/** Build the unified macOS installer: a productbuild distribution .pkg wrapping
 *  the signed client .app (component pkg into /Applications), the Core payload
 *  (staged under the system prefix), option-marker component pkgs, and a
 *  payload-free setup pkg whose postinstall reads the markers and provisions
 *  the Core. Signed + notarized through the same Installer-identity path as
 *  every pkg (secret-driven, unsigned fallback). Returns [] when the packaging
 *  tools are unavailable. Must run right after the client build, while the
 *  Tauri bundle dir (the .app) is still on disk. */
export async function buildMacUnified(
  built: CoreBuildArtifacts,
  opts: UnifiedInstallerOpts,
): Promise<InstallerAsset[]> {
  if (!(await have("pkgbuild")) || !(await have("productbuild"))) {
    info(colors.yellow(`pkgbuild/productbuild not available; skipping the unified installer`));
    return [];
  }
  const { channel, triple, env, coreVersion, clientVersion } = opts;
  const suffix = channelBinSuffix(channel);
  const ids = macPkgIds(channel);
  const prefix = macInstallPrefix(channel);
  const work = await Deno.makeTempDir({ prefix: "loca-unified-pkg-" });
  const appPath = await findMacApp(triple);
  const appName = basename(appPath);

  // Client component: wraps the (Developer ID-signed, when configured) .app
  // as-is; pkgbuild --component preserves the app signature.
  await run([
    "pkgbuild",
    "--component",
    appPath,
    "--install-location",
    "/Applications",
    "--identifier",
    ids.client,
    "--version",
    clientVersion,
    join(work, "client.pkg"),
  ]);

  // Core payload component: bin + workers (+ the offline built-in extension
  // plant when reachable) + the uninstaller, staged under the system prefix.
  const pkgRoot = join(work, "core-root");
  const { binName } = await assemblePayload(built, triple, channel, pkgRoot);
  await stageBuiltinExtension(pkgRoot, env, channel);
  const uninstall = renderMacUninstall(await Deno.readTextFile(MAC_UNINSTALL_TEMPLATE), {
    channel,
    prefix,
    appName,
    binName,
    pkgIds: Object.values(ids),
    // The launchd agent label install-service registers (service.ts), for the
    // binary-missing fallback teardown.
    serviceLabel: ids.core,
  });
  await Deno.writeTextFile(join(pkgRoot, "uninstall.sh"), uninstall);
  await Deno.chmod(join(pkgRoot, "uninstall.sh"), 0o755);
  await run([
    "pkgbuild",
    "--root",
    pkgRoot,
    "--identifier",
    ids.core,
    "--version",
    coreVersion,
    "--install-location",
    prefix,
    join(work, "core.pkg"),
  ]);

  // Option markers: one file each under <prefix>/opts/. Selecting the choice
  // installs the marker; the setup postinstall translates markers into the
  // LOCA_INSTALL_* env, so the GUI checkboxes and a headless
  // -applyChoiceChangesXML run share one mechanism.
  for (const [name, id] of [
    ["service", ids.optService],
    ["lan", ids.optLan],
    ["behindproxy", ids.optBehindProxy],
  ] as const) {
    const markerRoot = join(work, `opt-${name}-root`);
    await ensureDir(join(markerRoot, "opts"));
    await Deno.writeTextFile(join(markerRoot, "opts", name), "1\n");
    await run([
      "pkgbuild",
      "--root",
      markerRoot,
      "--identifier",
      id,
      "--version",
      coreVersion,
      "--install-location",
      prefix,
      join(work, `opt-${name}.pkg`),
    ]);
  }

  // Setup component: payload-free; its postinstall runs LAST (final line in the
  // choices outline) so every marker is on disk before it reads them.
  const setupScripts = join(work, "setup-scripts");
  await ensureDir(setupScripts);
  await Deno.writeTextFile(
    join(setupScripts, "postinstall"),
    macSetupPostinstall(channel, prefix, binName),
  );
  await Deno.chmod(join(setupScripts, "postinstall"), 0o755);
  await run([
    "pkgbuild",
    "--nopayload",
    "--identifier",
    ids.setup,
    "--version",
    coreVersion,
    "--scripts",
    setupScripts,
    join(work, "setup.pkg"),
  ]);

  const distXml = join(work, "distribution.xml");
  await Deno.writeTextFile(
    distXml,
    unifiedDistributionXml({ channel, coreVersion, clientVersion }),
  );
  const unsigned = join(work, `loca${suffix}-${clientVersion}-unsigned.pkg`);
  await run(["productbuild", "--distribution", distXml, "--package-path", work, unsigned]);

  // Same Installer-identity signing + notarization path as any pkg: productsign
  // requires a "Developer ID Installer" cert (a DIFFERENT kind from the
  // Application identity that signed the .app), imported into an ephemeral
  // keychain on CI (materializeInstallerKeychain). Secret-driven, with the
  // unsigned fallback notes.
  let finalPkg = unsigned;
  if (env.appleInstallerSigningIdentity) {
    const cleanupKeychain = await materializeInstallerKeychain(env);
    try {
      finalPkg = join(work, `loca${suffix}-${clientVersion}-signed.pkg`);
      await run(["productsign", "--sign", env.appleInstallerSigningIdentity, unsigned, finalPkg]);
      await notarizePkg(finalPkg, env);
    } finally {
      await cleanupKeychain();
    }
  } else if (env.appleSigningIdentity) {
    info(
      colors.yellow(
        `APPLE_INSTALLER_SIGNING_IDENTITY unset (APPLE_SIGNING_IDENTITY is an Application ` +
          `cert; productsign needs a Developer ID Installer cert); shipping an unsigned .pkg`,
      ),
    );
  } else {
    info(colors.yellow(`APPLE_INSTALLER_SIGNING_IDENTITY unset; shipping an unsigned .pkg`));
  }
  // Name the published artifact stably (drop the -signed/-unsigned suffix).
  const published = join(work, `loca${suffix}-${clientVersion}.pkg`);
  await Deno.rename(finalPkg, published);
  return [await record(published, triple, "unified", "pkg", clientVersion)];
}

/** Substitute the build-time tokens into the tracked mac uninstaller template.
 *  Exported for tests. */
export function renderMacUninstall(
  template: string,
  subs: {
    channel: ReleaseChannel;
    prefix: string;
    appName: string;
    binName: string;
    pkgIds: string[];
    serviceLabel: string;
  },
): string {
  return template
    .replaceAll("__CHANNEL__", subs.channel)
    .replaceAll("__PREFIX__", subs.prefix)
    .replaceAll("__APP_NAME__", subs.appName)
    .replaceAll("__BIN_NAME__", subs.binName)
    .replaceAll("__PKG_IDS__", subs.pkgIds.join(" "))
    .replaceAll("__SERVICE_LABEL__", subs.serviceLabel);
}

/** The setup component's postinstall: runs as root AFTER every payload
 *  component, reads the option markers the selected choices dropped under
 *  <prefix>/opts/, copies the Core payload into the console user's
 *  ~/.loca/<channel>/core, and provisions via the binary's own
 *  install-service as that user. Exported for tests. */
export function macSetupPostinstall(
  channel: ReleaseChannel,
  prefix: string,
  binName: string,
): string {
  return `#!/bin/bash
set -euo pipefail
PREFIX="${prefix}"
# The Core choice was deselected: no payload, nothing to provision. Drop any
# markers so a stray opts/ dir never outlives this run.
if [ ! -d "$PREFIX/bin" ]; then
  rm -rf "$PREFIX/opts"
  exit 0
fi
SERVICE=0
BINDALL=0
BEHINDPROXY=0
if [ -f "$PREFIX/opts/service" ]; then SERVICE=1; fi
if [ -f "$PREFIX/opts/lan" ]; then BINDALL=1; fi
if [ -f "$PREFIX/opts/behindproxy" ]; then BEHINDPROXY=1; fi
rm -rf "$PREFIX/opts"
# The install is per-user; target the console user, or - a headless install over
# SSH has no console session (/dev/console stats as root) - whoever ran sudo.
CONSOLE_USER="$(stat -f%Su /dev/console)"
if [ -z "$CONSOLE_USER" ] || [ "$CONSOLE_USER" = "root" ]; then
  CONSOLE_USER="\${SUDO_USER:-}"
fi
if [ -z "$CONSOLE_USER" ] || [ "$CONSOLE_USER" = "root" ]; then
  echo "no console user; skipping per-user service registration" >&2
  exit 0
fi
# sed, not awk '{print $2}': a home dir containing spaces must survive whole.
USER_HOME="$(dscl . -read "/Users/$CONSOLE_USER" NFSHomeDirectory | sed 's/^[^ ]* //')"
DEST="$USER_HOME/.loca/${channel}/core"
mkdir -p "$DEST/bin" "$DEST/workers"
cp -R "$PREFIX/bin/." "$DEST/bin/"
cp -R "$PREFIX/workers/." "$DEST/workers/"
if [ -d "$PREFIX/extensions" ]; then
  mkdir -p "$DEST/extensions"
  cp -R "$PREFIX/extensions/." "$DEST/extensions/"
fi
chown -R "$CONSOLE_USER" "$USER_HOME/.loca/${channel}"
# On a first-ever install the mkdir above created ~/.loca itself as root; the
# user's install-service must be able to create the shared models dir in it.
chown "$CONSOLE_USER" "$USER_HOME/.loca"
# Run install-service AS the console user with HOME pinned to their home: sudo's
# env_reset would otherwise leave HOME as root's, so the LaunchAgent plist +
# admin token would land in /var/root instead of the user home. The env prefix
# guarantees the vars reach the child regardless of the sudoers setenv policy.
sudo -u "$CONSOLE_USER" env HOME="$USER_HOME" LOCA_CHANNEL="${channel}" LOCA_INSTALL_SERVICE="$SERVICE" LOCA_INSTALL_BIND_ALL="$BINDALL" LOCA_INSTALL_BEHIND_PROXY="$BEHINDPROXY" "$DEST/bin/${binName}" install-service || true
exit 0
`;
}

/** The distribution file: the choice tree Installer.app renders (and
 *  -applyChoiceChangesXML overrides). Exported for tests.
 *
 *  - `client` is preselected and disabled (required in the GUI); a headless
 *    core-only install re-enables then deselects it via choices XML.
 *  - `core` is preselected and deselectable.
 *  - The option choices are marker component pkgs (see buildMacUnified); their
 *    dynamic `selected` JS ties them to the core choice so deselecting the
 *    Core drops its options too. `core-behindproxy` is hidden (server-install
 *    option, headless-only via choices XML). Known limit of the
 *    self-referencing `selected` expression: toggling Core off and back on
 *    leaves the option boxes wherever the toggle-off put them (Installer.app
 *    re-evaluates `.selected` against their now-false state), so that
 *    round-trip loses the service-on default; re-checking it by hand works.
 *  - `core-setup` is the hidden always-with-core final component whose
 *    postinstall provisions the Core; it is last in the outline so it runs
 *    after every marker is on disk. */
export function unifiedDistributionXml(o: {
  channel: ReleaseChannel;
  coreVersion: string;
  clientVersion: string;
}): string {
  const ids = macPkgIds(o.channel);
  const title = unifiedDisplayName(o.channel);
  return `<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
  <title>${title}</title>
  <options customize="allow" require-scripts="false" rootVolumeOnly="true"/>
  <domains enable_localSystem="true"/>
  <choices-outline>
    <line choice="client"/>
    <line choice="core">
      <line choice="core-service"/>
      <line choice="core-lan"/>
      <line choice="core-behindproxy"/>
    </line>
    <line choice="core-setup"/>
  </choices-outline>
  <choice id="client" title="Loca Client" description="The Loca app, installed into Applications." start_selected="true" start_enabled="false">
    <pkg-ref id="${ids.client}"/>
  </choice>
  <choice id="core" title="Loca Core" description="The service that runs your models and tools on this computer. Uncheck it to connect the Client to a Core on another computer instead." start_selected="true">
    <pkg-ref id="${ids.core}"/>
  </choice>
  <choice id="core-service" title="Keep the Core running in the background" description="Starts the Core at login so it is always ready. When off, the Client starts it on demand." start_selected="true" enabled="choices.core.selected" selected="choices.core.selected &amp;&amp; choices['core-service'].selected">
    <pkg-ref id="${ids.optService}"/>
  </choice>
  <choice id="core-lan" title="Allow other devices on this network to pair" description="When off, only this computer can reach the Core." start_selected="false" enabled="choices.core.selected" selected="choices.core.selected &amp;&amp; choices['core-lan'].selected">
    <pkg-ref id="${ids.optLan}"/>
  </choice>
  <choice id="core-behindproxy" title="Behind an HTTPS proxy" description="Trust a proxy's certificate instead of pinning the Core's own." visible="false" start_selected="false" selected="choices.core.selected &amp;&amp; choices['core-behindproxy'].selected">
    <pkg-ref id="${ids.optBehindProxy}"/>
  </choice>
  <choice id="core-setup" title="Core setup" visible="false" selected="choices.core.selected">
    <pkg-ref id="${ids.setup}"/>
  </choice>
  <pkg-ref id="${ids.client}" version="${o.clientVersion}" onConclusion="none">client.pkg</pkg-ref>
  <pkg-ref id="${ids.core}" version="${o.coreVersion}" onConclusion="none">core.pkg</pkg-ref>
  <pkg-ref id="${ids.optService}" version="${o.coreVersion}" onConclusion="none">opt-service.pkg</pkg-ref>
  <pkg-ref id="${ids.optLan}" version="${o.coreVersion}" onConclusion="none">opt-lan.pkg</pkg-ref>
  <pkg-ref id="${ids.optBehindProxy}" version="${o.coreVersion}" onConclusion="none">opt-behindproxy.pkg</pkg-ref>
  <pkg-ref id="${ids.setup}" version="${o.coreVersion}" onConclusion="none">setup.pkg</pkg-ref>
</installer-gui-script>
`;
}
