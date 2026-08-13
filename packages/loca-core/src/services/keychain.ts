// Thin wrapper around the `loca-core-keychain` native helper binary.
//
// The helper exposes the platform keychain (macOS Keychain, Linux libsecret
// via secret-service, Windows Credential Manager) through a stdio CLI so
// the Deno core doesn't have to bind to native FFI directly.
//
// Returns `null` on either "no such entry" (helper exits 1) OR "helper not
// available / errored" (binary missing, libsecret unavailable on headless
// Linux, etc). Callers MUST handle null with a non-keychain fallback.

import { binPath } from "../paths.ts";
import { errMessage } from "@loca/shared";
import { coreBinaryName } from "../binaries/versions.ts";
import { getLogger } from "../shared/log.ts";

const log = getLogger("keychain");

const EXIT_ENTRY_MISSING = 1;

function helperPath(): string {
  return binPath(coreBinaryName("loca-core-keychain"));
}

async function helperExists(): Promise<boolean> {
  try {
    await Deno.stat(helperPath());
    return true;
  } catch {
    return false;
  }
}

/** Reads a keychain entry. Returns null on missing entry or any keychain
 *  failure (helper missing, libsecret unavailable, etc). */
export async function keychainGet(service: string, account: string): Promise<string | null> {
  if (!(await helperExists())) return null;
  const cmd = new Deno.Command(helperPath(), {
    args: ["get", service, account],
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
  });
  try {
    const { code, stdout, stderr } = await cmd.output();
    if (code === 0) {
      // Helper appends a trailing newline.
      return new TextDecoder().decode(stdout).replace(/\n$/, "");
    }
    if (code === EXIT_ENTRY_MISSING) return null;
    log.warn(
      `keychain get(${service}/${account}) exited ${code}: ` +
        new TextDecoder().decode(stderr).trim(),
    );
    return null;
  } catch (err) {
    log.warn(`keychain helper spawn failed: ${errMessage(err)}`);
    return null;
  }
}

/** Writes a keychain entry. Returns true on success, false on any failure
 *  (helper missing, libsecret unavailable, permission denied, etc). */
export async function keychainSet(
  service: string,
  account: string,
  value: string,
): Promise<boolean> {
  if (!(await helperExists())) return false;
  const cmd = new Deno.Command(helperPath(), {
    args: ["set", service, account],
    stdin: "piped",
    stdout: "null",
    stderr: "piped",
  });
  try {
    const child = cmd.spawn();
    const writer = child.stdin.getWriter();
    await writer.write(new TextEncoder().encode(value));
    await writer.close();
    const status = await child.status;
    if (status.success) return true;
    // Drain ALL of stderr (the previous single read() truncated a multi-chunk
    // error to its first chunk, losing the actual failure detail).
    const stderrText = await new Response(child.stderr).text().catch(() => "");
    log.warn(`keychain set(${service}/${account}) exited ${status.code}: ${stderrText.trim()}`);
    return false;
  } catch (err) {
    log.warn(`keychain helper spawn failed: ${errMessage(err)}`);
    return false;
  }
}

/** Removes a keychain entry. Idempotent: returns true if the entry is gone
 *  after the call (whether it existed or not), false on a real failure. */
export async function keychainDelete(service: string, account: string): Promise<boolean> {
  if (!(await helperExists())) return false;
  const cmd = new Deno.Command(helperPath(), {
    args: ["delete", service, account],
    stdin: "null",
    stdout: "null",
    stderr: "piped",
  });
  try {
    const { code, stderr } = await cmd.output();
    if (code === 0) return true;
    log.warn(
      `keychain delete(${service}/${account}) exited ${code}: ` +
        new TextDecoder().decode(stderr).trim(),
    );
    return false;
  } catch (err) {
    log.warn(`keychain helper spawn failed: ${errMessage(err)}`);
    return false;
  }
}
