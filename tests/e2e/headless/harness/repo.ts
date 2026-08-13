// Shared path constants for the headless harness (Node side).
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
// harness/ -> headless/ -> e2e/ -> tests/ -> repo root
export const REPO_ROOT = resolve(here, "..", "..", "..", "..");
export const CORE_ENTRY = resolve(REPO_ROOT, "packages/loca-core/src/main.ts");
export const TARGET_DEBUG = resolve(REPO_ROOT, "target/debug");
// The tool-worker entry deno runs for every extension tool. A real install
// stages it into <home>/workers via self-update; running core from source has
// no such step, so point at the source dir (the same override `deno task dev`
// uses). Without it the worker entry is missing and every tool call fails fast.
export const WORKERS_DIR = resolve(REPO_ROOT, "packages/loca-core/src/workers");

// The four native helper binaries core's boot-time check requires. We symlink
// them from target/debug into each test core's bin dir (dev channel suffix
// "-dev") so the real boot path runs without a source-level skip.
export const REQUIRED_HELPERS = [
  "loca-core-keychain",
  "loca-core-updater",
  "loca-core-hwinfo",
  "loca-core-ptyhost",
];

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
