// Boot-time presence check for the native helper binaries core depends on.
//
// These ship in the signed release manifest and are placed in the bin dir by
// the native installer's payload or, in dev, by `deno task dev`
// (scripts/dev.ts provisionHelpers). Core refuses to start if any are missing
// rather than silently degrading: each helper has a quiet runtime fallback that
// is appropriate when the helper RUNS but its OS facility is unavailable (e.g.
// the keychain helper present but libsecret missing on Linux), NOT when the
// binary itself is absent. A missing binary means a broken install, and the
// fallbacks would hide it: file-backed secrets instead of the OS keychain, a
// guessed hardware profile, and tool workers spawned without permission prompts
// (ask-state permissions silently failing). Fail loud at boot instead.

import { paths } from "../paths.ts";
import { binPath } from "../paths.ts";
import { coreBinaryName } from "./versions.ts";
import { AppError } from "@loca/core-engine";

// Channel-independent base names. ptyhost is required on every platform: unix
// uses a PTY, Windows a ConPTY (see loca-core-ptyhost), and both surface Deno's
// permission prompts through the same protocol.
function requiredHelpers(): string[] {
  return ["loca-core-keychain", "loca-core-updater", "loca-core-hwinfo", "loca-core-ptyhost"];
}

/** Throw if any required helper binary is absent from the bin dir. Called early
 *  in boot so a broken install fails with a clear error instead of degrading. */
export function ensureHelperBinaries(): void {
  const missing: string[] = [];
  for (const name of requiredHelpers()) {
    const path = binPath(coreBinaryName(name));
    try {
      if (!Deno.statSync(path).isFile) missing.push(name);
    } catch {
      missing.push(name);
    }
  }
  if (missing.length === 0) return;
  throw new AppError(
    "internal_error",
    `missing helper binaries in ${paths().binDir}: ${missing.join(", ")}. ` +
      `They ship in the release manifest and are installed alongside core; in ` +
      `dev, \`deno task dev\` builds and links them. Refusing to start rather ` +
      `than silently fall back (file-backed secrets, guessed hardware, tool ` +
      `workers without permission prompts).`,
  );
}
