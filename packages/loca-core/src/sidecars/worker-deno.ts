// The sandboxed tool workers and the npm-based extension installer run as
// `deno run` subprocesses using the bundled `deno` sidecar binary. That binary
// is a downloadable requirement (see requiredBinaryKinds): until the user
// installs it, spawning would throw a raw `NotFound` that is easy to leak as an
// uncaught rejection and kill the core. Resolve + existence-check it here so
// callers get a clean, handleable error instead.

import { binPath } from "../paths.ts";
import { binaryName } from "../binaries/versions.ts";
import { AppError } from "@loca/core-engine";

export async function requireWorkerDeno(): Promise<string> {
  const p = binPath(binaryName("deno"));
  try {
    if ((await Deno.stat(p)).isFile) return p;
  } catch {
    // fall through to the not-installed error
  }
  throw new AppError(
    "binary_not_found",
    `the 'deno' worker runtime is not installed yet; download required files from Settings`,
  );
}
