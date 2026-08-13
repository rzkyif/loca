// Cross-platform ("all targets") engine shared by `deno task build` and
// `deno task release`.
//
// A complete release spans 6 triples across 3 OSes plus Android. The speech
// sidecar (static sherpa-onnx) and the Tauri installers can't be cross-compiled,
// so each OS is built NATIVELY in its own environment, started on demand from
// this M4 host:
//
//   darwin   -> the host itself (same-OS cross-arch: both apple-darwin arches)
//   linux    -> a Podman container        (Phase 2)
//   windows  -> the UTM Windows VM         (Phase 3)
//
// This module resolves which requested triples can be built on this machine
// right now. Triples whose environment is not yet wired are reported as skipped
// (the manifests tolerate a partial platform set, so a partial run publishes
// what it built and a later run fills the gaps).

import type { Triple } from "../../packages/loca-shared/src/domain/model.ts";
import { TRIPLES } from "../../packages/loca-shared/src/domain/model.ts";
import { colors, info } from "./lib.ts";
import type { BuildEnvironment } from "./drivers/mod.ts";

/** What `deno task release`/`build` target by default (--triples=all). A curated
 *  subset of TRIPLES: every realistic desktop arch, minus linux-arm64 (niche for
 *  a desktop GUI; ARM Linux is mostly headless/servers). Re-add it here if that
 *  demand appears. An explicit --triples=<csv> can still request any triple the
 *  system knows (validated against ALL_KNOWN_TRIPLES). */
export const RELEASE_TARGET_TRIPLES: Triple[] = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "aarch64-pc-windows-msvc",
  "x86_64-unknown-linux-gnu",
];

/** Every triple the system knows, for validating explicit --triples entries. */
export const ALL_KNOWN_TRIPLES: Triple[] = [...TRIPLES];

type Os = "darwin" | "linux" | "windows";

function tripleOs(triple: Triple): Os {
  if (triple.includes("apple-darwin")) return "darwin";
  if (triple.includes("windows")) return "windows";
  return "linux";
}

function hostOs(): Os {
  return Deno.build.os as Os;
}

// How each non-host OS will eventually be built. Used only to print a helpful
// "skipped, needs X" line until that driver is wired.
const PENDING_DRIVER: Record<Os, string> = {
  darwin: "host",
  linux: "Podman Linux driver (Phase 2)",
  windows: "UTM Windows driver (Phase 3)",
};

export interface ResolvedTriples {
  /** Triples buildable on this machine now (same OS as the host). */
  build: Triple[];
  /** Requested triples whose environment is not yet available, with the reason. */
  skipped: Array<{ triple: Triple; reason: string }>;
}

/** Split the requested triples into what this host can build now vs. what needs
 *  a not-yet-wired environment. Same-OS cross-arch builds natively, so on macOS
 *  both apple-darwin arches are buildable; linux/windows wait for their driver. */
export function resolveBuildableTriples(requested: Triple[]): ResolvedTriples {
  const host = hostOs();
  const build: Triple[] = [];
  const skipped: ResolvedTriples["skipped"] = [];
  for (const triple of requested) {
    if (tripleOs(triple) === host) build.push(triple);
    else skipped.push({ triple, reason: PENDING_DRIVER[tripleOs(triple)] });
  }
  return { build, skipped };
}

/** Print the skipped triples so a partial cross-platform run is never silent
 *  about what it didn't build. */
export function reportSkipped(skipped: ResolvedTriples["skipped"]): void {
  if (skipped.length === 0) return;
  info(
    colors.yellow(
      `Skipping ${skipped.length} triple(s) this run (no native environment on this host yet):`,
    ),
  );
  for (const { triple, reason } of skipped) {
    info(colors.yellow(`  ${triple} -> ${reason}`));
  }
}

export interface TripleRouting {
  /** Built directly on this host (same OS, native or same-OS cross-arch). */
  host: Triple[];
  /** Routed to an on-demand build environment (started for the build, then stopped). */
  byEnv: Array<{ env: BuildEnvironment; triples: Triple[] }>;
  /** Couldn't be built: no environment owns the triple, or its driver is unconfigured. */
  skipped: Array<{ triple: Triple; reason: string }>;
}

/** Assign each requested triple to where it gets built: the host (same OS), an
 *  available driver environment, or the skipped list. Driver availability is
 *  probed (available()), so an unconfigured driver degrades to "skipped" rather
 *  than failing the run. */
export async function routeTriples(
  requested: Triple[],
  environments: BuildEnvironment[],
): Promise<TripleRouting> {
  const host = hostOs();
  const hostTriples: Triple[] = [];
  const byEnvMap = new Map<BuildEnvironment, Triple[]>();
  const skipped: TripleRouting["skipped"] = [];
  for (const triple of requested) {
    if (tripleOs(triple) === host) {
      hostTriples.push(triple);
      continue;
    }
    const env = environments.find((e) => e.triples.includes(triple));
    if (env && (await env.available())) {
      byEnvMap.set(env, [...(byEnvMap.get(env) ?? []), triple]);
    } else {
      skipped.push({
        triple,
        reason: env ? `${env.id} unavailable (unconfigured?)` : PENDING_DRIVER[tripleOs(triple)],
      });
    }
  }
  return {
    host: hostTriples,
    byEnv: [...byEnvMap].map(([env, triples]) => ({ env, triples })),
    skipped,
  };
}

/** Announce where each triple builds, so a cross-platform run is legible. */
export function reportRouting(routing: TripleRouting): void {
  if (routing.host.length) info(`host builds: ${routing.host.join(", ")}`);
  for (const { env, triples } of routing.byEnv) info(`${env.id} builds: ${triples.join(", ")}`);
  reportSkipped(routing.skipped);
}
