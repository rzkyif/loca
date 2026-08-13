// Unit tests for the Windows Add/Remove Programs UninstallString builder (the
// self-elevation for a service install is what lets a Settings > Apps uninstall
// remove the root-folder Scheduled Task, which needs admin; a background
// install has no task and must not prompt), the ARP-ownership skip the unified
// installer relies on, and the --purge-models directory removal.

import { assertEquals, assertMatch, assertStringIncludes } from "@std/assert";
import { join } from "@std/path";
import {
  buildUninstallCommand,
  encodePwshCommand,
  parsePid,
  removeModelsDir,
  shouldRegisterArpEntry,
} from "./service.ts";

/** Decode a PowerShell -EncodedCommand payload (base64 of UTF-16LE) back to a
 *  string, mirroring what powershell.exe does when it runs the elevated relaunch. */
function decodePwshCommand(b64: string): string {
  const binary = atob(b64);
  let out = "";
  for (let i = 0; i < binary.length; i += 2) {
    out += String.fromCharCode(binary.charCodeAt(i) | (binary.charCodeAt(i + 1) << 8));
  }
  return out;
}

Deno.test("encodePwshCommand round-trips through the -EncodedCommand decoder", () => {
  for (const s of ["", "hi", "$env:X='a'; & 'C:\\a b\\c.exe' go", "high codepoint π pi"]) {
    assertEquals(decodePwshCommand(encodePwshCommand(s)), s);
  }
  // Pin the exact wire format PowerShell -EncodedCommand expects (base64 of
  // UTF-16LE): matches [Convert]::ToBase64String([Text.Encoding]::Unicode...).
  assertEquals(encodePwshCommand("hi"), "aABpAA==");
});

const OPTS = {
  bin: "C:\\Users\\u\\.tomat\\latest\\core\\bin\\tomat-core-latest.exe",
  root: "C:\\Users\\u\\.tomat\\latest\\core",
  channelDir: "C:\\Users\\u\\.tomat\\latest",
  ch: "latest",
};

Deno.test("background install: runs the teardown directly, no elevation prompt", () => {
  const cmd = buildUninstallCommand({ asService: false, ...OPTS });
  assertStringIncludes(cmd, "-Command \"$env:TOMAT_CHANNEL='latest'");
  assertStringIncludes(cmd, "uninstall-service");
  // No self-elevation for a background install (nothing needs admin).
  assertEquals(cmd.includes("Start-Process"), false);
  assertEquals(cmd.includes("RunAs"), false);
  assertEquals(cmd.includes("-EncodedCommand"), false);
  // Empty channel dir is swept (the residue the inline ARP command used to leave).
  assertStringIncludes(cmd, "Get-ChildItem -Force -LiteralPath $cd");
});

Deno.test("service install: self-elevates and carries the teardown as -EncodedCommand", () => {
  const cmd = buildUninstallCommand({ asService: true, ...OPTS });
  assertStringIncludes(cmd, "Start-Process powershell.exe -Verb RunAs -Wait");
  assertMatch(cmd, /-EncodedCommand','[A-Za-z0-9+/=]+'/);
  // The encoded payload is the same teardown the background path runs inline.
  const b64 = cmd.match(/-EncodedCommand','([A-Za-z0-9+/=]+)'/)![1];
  const inner = decodePwshCommand(b64);
  assertStringIncludes(inner, "$env:TOMAT_CHANNEL='latest'");
  assertStringIncludes(inner, "uninstall-service");
  assertStringIncludes(inner, OPTS.root);
  assertStringIncludes(inner, "Get-ChildItem -Force -LiteralPath $cd");
});

Deno.test("paths with a single quote are escaped for the single-quoted PS literal", () => {
  const cmd = buildUninstallCommand({
    asService: false,
    bin: "C:\\o'brien\\core.exe",
    root: "C:\\o'brien\\core",
    channelDir: "C:\\o'brien",
    ch: "latest",
  });
  // Single quote doubled (PowerShell's single-quote escape), never left bare.
  assertStringIncludes(cmd, "C:\\o''brien\\core.exe");
});

Deno.test("ARP registration skips on exactly TOMAT_INSTALL_REGISTER_ARP=0", () => {
  // Only the unified installer's explicit "0" hands the entry over; anything
  // else (unset included) keeps install-service as the entry's owner.
  assertEquals(shouldRegisterArpEntry("0"), false);
  assertEquals(shouldRegisterArpEntry(undefined), true);
  assertEquals(shouldRegisterArpEntry("1"), true);
  assertEquals(shouldRegisterArpEntry("false"), true);
});

Deno.test("parsePid reads the PID startBackground echoes", () => {
  // unix: `echo $!` on its own stdout line; windows: the temp pid file.
  assertEquals(parsePid("4242\n"), 4242);
  assertEquals(parsePid("  4242  \r\n"), 4242);
});

Deno.test("parsePid returns null rather than a bogus PID", () => {
  // A null here only means "we don't own this core's liveness", which is safe.
  // A wrong number would have the client SIGTERM an unrelated process on exit.
  for (const raw of ["", "\n", "not a pid", "0", "-1", "12.5", "NaN", "Infinity"]) {
    assertEquals(parsePid(raw), null, `expected null for ${JSON.stringify(raw)}`);
  }
});

Deno.test("removeModelsDir removes only the shared models dir", async () => {
  // paths() resolves everything under TOMAT_CORE_HOME, so point it at a temp
  // tree shaped like a real ~/.tomat and check the surgical removal.
  const home = await Deno.makeTempDir({ prefix: "tomat-models-test-" });
  const prev = Deno.env.get("TOMAT_CORE_HOME");
  Deno.env.set("TOMAT_CORE_HOME", home);
  try {
    await Deno.mkdir(join(home, "models", "llm"), { recursive: true });
    await Deno.writeTextFile(join(home, "models", "llm", "weights.gguf"), "w");
    await Deno.mkdir(join(home, "stable", "core"), { recursive: true });
    await Deno.writeTextFile(join(home, "stable", "core", "settings.json"), "{}");

    await removeModelsDir();
    assertEquals(await exists(join(home, "models")), false);
    // The channel data next to it is untouched.
    assertEquals(await exists(join(home, "stable", "core", "settings.json")), true);

    // Idempotent: a second run on the already-gone dir is a no-op, not a throw.
    await removeModelsDir();
  } finally {
    if (prev === undefined) Deno.env.delete("TOMAT_CORE_HOME");
    else Deno.env.set("TOMAT_CORE_HOME", prev);
    await Deno.remove(home, { recursive: true }).catch(() => {});
  }
});

async function exists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch {
    return false;
  }
}
