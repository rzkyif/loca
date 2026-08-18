// Repo-relative paths and per-target binary resolution.

import { fromFileUrl, join, resolve } from "@std/path";

// harness/ -> conformance/ -> tests/ -> repo root
export const REPO_ROOT = resolve(fromFileUrl(import.meta.url), "..", "..", "..", "..");

/** The Deno core's entrypoint, run from source. */
export const DENO_CORE_ENTRY = join(REPO_ROOT, "packages/loca-core/src/main.ts");

/** The tool-worker entry deno spawns for every extension tool. A real install
 *  stages it into <home>/workers during self-update; running from source has no
 *  such step, so point at the source dir the same way `deno task dev` does. */
export const WORKERS_DIR = join(REPO_ROOT, "packages/loca-core/src/workers");

/** Native helper binaries the Deno core's boot check requires to be PRESENT.
 *  It stats them and never executes them at boot, and the dev channel never
 *  uses the OS keychain, so stubs are enough for everything that does not
 *  actually invoke a helper. See `seedHome`. */
export const REQUIRED_HELPERS = [
  "loca-core-keychain",
  "loca-core-updater",
  "loca-core-hwinfo",
  "loca-core-ptyhost",
];

export interface RustBinary {
  path: string;
  profile: "release" | "debug";
  mtime: Date | null;
}

/** The newest mtime under the Rust core's sources, or null if none can be read.
 *
 *  Compared against the binary's mtime by [`rustCoreStaleness`]. Walks the crate
 *  source plus the files that change what a build produces without living under
 *  `src/` (the manifests, the generated contract's inputs are inside `src/`). */
function newestRustSourceMtime(): { at: Date; file: string } | null {
  const roots = [
    join(REPO_ROOT, "packages/loca-core-rs/src"),
    join(REPO_ROOT, "packages/loca-core-rs/Cargo.toml"),
    join(REPO_ROOT, "Cargo.toml"),
    join(REPO_ROOT, "Cargo.lock"),
  ];
  let newest: { at: Date; file: string } | null = null;
  const consider = (path: string, mtime: Date | null) => {
    if (mtime && (!newest || mtime > newest.at)) newest = { at: mtime, file: path };
  };
  const walk = (dir: string) => {
    for (const entry of Deno.readDirSync(dir)) {
      const path = join(dir, entry.name);
      if (entry.isDirectory) walk(path);
      else if (entry.isFile) consider(path, Deno.statSync(path).mtime);
    }
  };
  for (const root of roots) {
    try {
      const stat = Deno.statSync(root);
      if (stat.isDirectory) walk(root);
      else consider(root, stat.mtime);
    } catch {
      // A missing root is not an error: the check is best-effort and must never
      // be the reason a run cannot start.
    }
  }
  return newest;
}

/** The source file newer than `binary`, or null when the build is current.
 *
 *  The runner does NOT build. Without this, editing the crate and re-running
 *  conformance silently re-measures the PREVIOUS binary and reports the same
 *  numbers, so a change looks verified when nothing exercised it. That has
 *  already happened once. Absence of the binary was caught from the start;
 *  staleness was not, and staleness is the worse of the two because it produces
 *  a confident green rather than an error. */
export function rustCoreStaleness(binary: RustBinary): { at: Date; file: string } | null {
  if (!binary.mtime) return null;
  const newest = newestRustSourceMtime();
  return newest && newest.at > binary.mtime ? newest : null;
}

/** Locate the Rust core binary, preferring release when both exist. The run
 *  header prints the result: conformance passing against a stale debug build is
 *  the failure this exists to make visible. */
export function findRustCore(): RustBinary | null {
  const exe = Deno.build.os === "windows" ? "loca-core.exe" : "loca-core";
  for (const profile of ["release", "debug"] as const) {
    const path = join(REPO_ROOT, "target", profile, exe);
    try {
      const stat = Deno.statSync(path);
      if (stat.isFile) return { path, profile, mtime: stat.mtime };
    } catch {
      // not built in this profile
    }
  }
  return null;
}
