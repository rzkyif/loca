// Extension installer public API + orchestration of its two user-triggered phases:
//   Download: fetch tarball / copy folder -> validate loca.json -> upsert rows
//             (status 'downloaded'). The folder is left byte-identical to what
//             was downloaded; we never edit deno.json. (installer-download.ts)
//   Install:  run `deno install` for any declared deps -> pin content hash ->
//             flip to status 'installed'. (installer-deps.ts)
//
// Registry writes shared by both phases live in installer-register.ts; shared
// types + tiny helpers in installer-shared.ts. The module graph is a DAG:
// installer -> download -> register -> deps, all -> shared.
//
// Caller passes an `EventSink` to receive install_log + install_done frames for
// forwarding to the requesting client over WS.
import { errMessage } from "@loca/shared";
import { AppError } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { newJobId } from "@loca/core-engine";
import { flattenNpmName, type InstallEventSink, type InstallSource } from "./installer-shared.ts";
import { runDownload } from "./installer-download.ts";
import { runInstallDeps } from "./installer-deps.ts";

// Re-exported public surface (callers + tests import these from here).
export { isWithin } from "@loca/core-engine";
export { flattenPermissions, registerLocalDownloaded } from "./installer-register.ts";
export { extractableEntryType, verifyTarball } from "./installer-download.ts";
export { flattenNpmName };
export type { InstallEventSink, InstallSource };

const log = getLogger("extension-installer");

export interface InstallStarted {
  jobId: string;
  extensionId: string;
}

const NOOP_SINK: InstallEventSink = {
  log() {
    /* */
  },
  done() {
    /* */
  },
};

// Per-extension in-flight guard. startDownload/startInstallDeps/startUpdate are
// fire-and-forget and all derive identical `.new`/`.old` staging paths from the
// extensionId, so two concurrent jobs for the same id race: one job's rmrf of the
// staging dir can wipe another's half-written extract, and two swaps interleave
// and strand the old version. Refuse a second start while one is in flight.
const inFlightExtensions = new Set<string>();

function beginExclusive(extensionId: string): void {
  if (inFlightExtensions.has(extensionId)) {
    throw new AppError("conflict", `an install/update for "${extensionId}" is already in progress`);
  }
  inFlightExtensions.add(extensionId);
}

// A local-install slug becomes a filesystem path segment under the extensions
// dir; constrain it to a safe identifier charset so it can't contain `.`/`..`
// or separators that would escape the dir.
const SLUG_RE = /^[a-zA-Z0-9_-]{1,64}$/;

// Phase 1 (download); see the module header. Caller then triggers startInstallDeps.
export function startDownload(
  spec: InstallSource,
  sink: InstallEventSink = NOOP_SINK,
): InstallStarted {
  const jobId = newJobId();
  if (spec.source === "local" && !SLUG_RE.test(spec.slug)) {
    throw new AppError(
      "validation_error",
      `invalid extension slug "${spec.slug}"; allowed: ${SLUG_RE.source}`,
    );
  }
  const extensionId = extensionIdForSpec(spec);
  beginExclusive(extensionId);
  void runDownload(spec, extensionId, jobId, sink)
    .then(() => sink.done(jobId, extensionId, true, 0))
    .catch((err) => {
      log.error(`download ${extensionId} failed: ${errMessage(err)}`);
      sink.done(jobId, extensionId, false, 1);
    })
    .finally(() => inFlightExtensions.delete(extensionId));
  return { jobId, extensionId };
}

// Phase 2 (install deps + pin content hash); see the module header.
export function startInstallDeps(
  extensionId: string,
  sink: InstallEventSink = NOOP_SINK,
): InstallStarted {
  const jobId = newJobId();
  beginExclusive(extensionId);
  void runInstallDeps(extensionId, jobId, sink)
    .then(() => sink.done(jobId, extensionId, true, 0))
    .catch((err) => {
      log.error(`install ${extensionId} failed: ${errMessage(err)}`);
      sink.done(jobId, extensionId, false, 1);
    })
    .finally(() => inFlightExtensions.delete(extensionId));
  return { jobId, extensionId };
}

// Update: re-download the latest bytes THEN re-install deps under one job, so an
// updated extension lands back in status='installed'. Because the install path
// re-pins the content hash, a legitimate update never trips drift.
export function startUpdate(
  spec: InstallSource,
  sink: InstallEventSink = NOOP_SINK,
): InstallStarted {
  const jobId = newJobId();
  const extensionId = extensionIdForSpec(spec);
  beginExclusive(extensionId);
  void runDownload(spec, extensionId, jobId, sink)
    .then(() => runInstallDeps(extensionId, jobId, sink))
    .then(() => sink.done(jobId, extensionId, true, 0))
    .catch((err) => {
      log.error(`update ${extensionId} failed: ${errMessage(err)}`);
      sink.done(jobId, extensionId, false, 1);
    })
    .finally(() => inFlightExtensions.delete(extensionId));
  return { jobId, extensionId };
}

function extensionIdForSpec(spec: InstallSource): string {
  return spec.source === "npm"
    ? flattenNpmName(spec.name)
    : spec.source === "seeded"
      ? spec.id
      : spec.slug;
}
