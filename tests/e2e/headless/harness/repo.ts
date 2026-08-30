// Shared path constants for the headless harness (Node side).
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { existsSync, statSync } from "node:fs";

const here = dirname(fileURLToPath(import.meta.url));
// harness/ -> headless/ -> e2e/ -> tests/ -> repo root
export const REPO_ROOT = resolve(here, "..", "..", "..", "..");
/** The freshest Rust core binary under target/{release,debug}. Helpers are
 *  taken from the same profile dir so a core never boots against a stale or
 *  missing helper from the other profile. */
export function findRustCore(): string {
  const exe = process.platform === "win32" ? "loca-core.exe" : "loca-core";
  const built = ["release", "debug"]
    .map((profile) => resolve(REPO_ROOT, "target", profile, exe))
    .filter((path) => existsSync(path))
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  if (!built.length) {
    throw new Error("no loca-core binary under target/; run `deno task build:core-rs` first");
  }
  return built[0];
}
// The tool-worker entry deno runs for every extension tool. A real install
// stages it into <home>/workers via self-update; running core from source has
// no such step, so point at the source dir (the same override `deno task dev`
// uses). Without it the worker entry is missing and every tool call fails fast.
export const WORKERS_DIR = resolve(REPO_ROOT, "packages/loca-core-rs/workers");

// The native helper binaries core's boot-time check requires, symlinked from
// the core's profile dir into each test core's bin dir (dev channel suffix "-dev").
export const REQUIRED_HELPERS = ["loca-core-updater"];

const HOME = process.env.HOME ?? process.env.USERPROFILE ?? "";

// Shared real weights live here; the mock download host streams from this cache
// when a requested model file is already present, else a synthetic fixture.
export const SHARED_MODELS_DIR = resolve(HOME, ".loca/models");

// The dev install's bin dir, where `deno` + `llama-server` sidecar binaries are
// staged. The harness symlinks these into a test core's bin dir so the
// requirements snapshot reports them present (happy-path scenarios). Models are
// satisfied by pointing the test models dir at SHARED_MODELS_DIR.
export const DEV_CORE_BIN = resolve(HOME, ".loca/dev/core/bin");
export const SIDECAR_BINARIES = ["deno", "llama-server"];
