// The single authoritative "required files" computation: given the current
// settings, what model files and sidecar binaries does the configuration need,
// and which are missing. The client renders this as one pending-downloads
// popup and gates the app on it. Recomputed + broadcast over WS whenever the
// inputs change (settings, a finished download, an installed binary).

import type { RequiredFile, RequirementsSnapshot } from "@loca/shared";
import {
  binarySource,
  binaryUnavailableOnTriple,
  requiredBinaryKinds,
  requiredModelRefs,
} from "@loca/shared";
import { loadCoreSettingsResolved } from "@loca/core-engine/services/core-settings";
import { modelsManager } from "../models/manager.ts";
import { binariesManager } from "../binaries/manager.ts";
import { downloadManager } from "../downloads/manager.ts";
import { hostTriple } from "../binaries/versions.ts";
import { getLogger } from "../shared/log.ts";

const log = getLogger("requirements");

/** Compute the full required-files list (models + binaries) and the missing
 *  subset for the current settings. Presence ("present"/"missing") is a purely
 *  local check. Size/version HEADs to HuggingFace/GitHub happen ONLY when
 *  `probeSizes` is set, so the always-on snapshot (client connect, settings
 *  broadcast) makes no outbound request per the no-non-consented-network rule;
 *  the size hints are filled in later, only when the user opens the Pending
 *  Downloads confirmation modal. */
export async function computeRequirements(
  opts: { probeSizes?: boolean } = {},
): Promise<RequirementsSnapshot> {
  const probeSizes = opts.probeSizes ?? false;
  const settings = await loadCoreSettingsResolved();

  // Models: dedupe by source (embed/tts base files are fixed; llm/stt could
  // theoretically collide). Keep the first group seen.
  const seen = new Set<string>();
  const modelRefs = requiredModelRefs(settings).filter((r) => {
    if (seen.has(r.source)) return false;
    seen.add(r.source);
    return true;
  });
  const modelProbes = await modelsManager().probe(
    modelRefs.map((r) => r.source),
    { network: probeSizes },
  );
  const probeBySource = new Map(modelProbes.map((p) => [p.source, p]));

  // A model download that failed lives on as an Error row in the queue, keyed by
  // the same source spec. Surface it as a retryable `error` so a stalled/failed
  // model download isn't a silent, perpetually-"downloading" gate (the binary
  // equivalent comes from the binaries manager below). Reading the live queue
  // keeps this self-consistent: a retry flips the row off Error and the error
  // clears on the next recompute, with nothing to reset by hand.
  const modelDownloadError = new Map<string, string>();
  for (const d of downloadManager().snapshot()) {
    if (d.destination === "models" && d.status === "Error") {
      modelDownloadError.set(d.source, d.error ?? "download failed");
    }
  }

  const required: RequiredFile[] = modelRefs.map((ref) => {
    const probe = probeBySource.get(ref.source);
    const present = probe?.alreadyHave ?? false;
    return {
      source: ref.source,
      type: "model",
      group: ref.group,
      present,
      error: present ? undefined : modelDownloadError.get(ref.source),
      sizeHint: present ? undefined : probe?.sizeHint,
    };
  });

  // Binaries: installed-ness is a local check; only probe (GitHub-cached) the
  // missing-and-available kinds for a version/size to show in the popup.
  const triple = hostTriple();
  const binKinds = requiredBinaryKinds(settings);
  const list = await binariesManager()
    .list()
    .catch(() => []);
  const statusByKind = new Map(list.map((b) => [b.kind, b]));
  // "Present" means installed AND the last install already AIMED for the variant
  // this device should have now (detected backend / override). We compare the
  // stored `target` (what the install aimed for), not the installed `variant`:
  // when the ideal (e.g. cuda) isn't resolvable upstream and the install degraded
  // to cpu, target still records cuda, so `target === desiredVariant` keeps it
  // present instead of looping the download popup on an unavoidable fallback. A
  // genuinely new ideal (GPU appeared, override changed) makes target differ from
  // desiredVariant, dropping it into `missing` for the reinstall. This is
  // network-free (no resolvability probe here); a better variant merely becoming
  // AVAILABLE upstream surfaces through the consented update path, not here. When
  // the desired/target can't be determined (offline / no manifest entry / not yet
  // tracked), a plain install counts as present so we never block on an
  // unknowable upgrade.
  const isPresent = (kind: (typeof binKinds)[number]): boolean => {
    const s = statusByKind.get(kind);
    if (!s || !s.installed) return false;
    if (!s.desiredVariant || !s.target) return true;
    return s.target === s.desiredVariant;
  };
  const missingAvailable = binKinds.filter(
    (k) => !isPresent(k) && !binaryUnavailableOnTriple(k, triple),
  );
  const binProbes =
    probeSizes && missingAvailable.length > 0
      ? await binariesManager()
          .probe(missingAvailable)
          .catch(() => [])
      : [];
  const binProbeByKind = new Map(binProbes.map((p) => [p.kind, p]));

  for (const kind of binKinds) {
    const present = isPresent(kind);
    const unavailable = !present && binaryUnavailableOnTriple(kind, triple);
    const probe = present ? undefined : binProbeByKind.get(kind);
    // A recorded install failure (couldn't resolve upstream, download/extract
    // errored) makes this a retryable error rather than a silent, sizeless,
    // perpetually-missing entry. Only meaningful while still missing.
    const error = !present && !unavailable ? binariesManager().failure(kind) : undefined;
    required.push({
      source: binarySource(kind),
      type: "binary",
      group: "binary",
      present,
      unavailable: unavailable || undefined,
      error,
      sizeHint: probe?.sizeBytes,
      version: probe && probe.version !== "unknown" ? probe.version : undefined,
    });
  }

  const missing = required.filter((r) => !r.present && !r.unavailable);
  return { required, missing };
}

// --- change notification ----------------------------------------------------

type RequirementsListener = (snap: RequirementsSnapshot) => void;
const listeners = new Set<RequirementsListener>();

/** Subscribe to recomputed requirements snapshots (the WS hub uses this to
 *  rebroadcast). */
export function onRequirementsChanged(fn: RequirementsListener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

let recomputeInFlight: Promise<void> | null = null;
let recomputeQueued = false;

/** Recompute and fan out to listeners. Called from the settings-change,
 *  download-completion, and binary-installed triggers. Fire-and-forget safe:
 *  a failed recompute logs and skips the broadcast rather than throwing.
 *
 *  Concurrent calls coalesce: this path recomputes from local presence checks
 *  only (no network - the size HEADs are deferred to the consented modal-open),
 *  but a settings batch can still fire many triggers, and each recompute walks
 *  the model/binary set and broadcasts. While a recompute runs, further calls
 *  flag a single trailing rerun and share the in-flight promise, bounding a
 *  burst to at most two recomputes. */
export function notifyRequirementsChanged(): Promise<void> {
  if (recomputeInFlight) {
    recomputeQueued = true;
    return recomputeInFlight;
  }
  recomputeInFlight = runRecompute().finally(() => {
    recomputeInFlight = null;
  });
  return recomputeInFlight;
}

async function runRecompute(): Promise<void> {
  do {
    recomputeQueued = false;
    let snap: RequirementsSnapshot;
    try {
      snap = await computeRequirements();
    } catch (err) {
      log.warn(`requirements recompute failed: ${err instanceof Error ? err.message : err}`);
      return;
    }
    for (const fn of listeners) {
      try {
        fn(snap);
      } catch (err) {
        log.warn(`requirements listener failed: ${err instanceof Error ? err.message : err}`);
      }
    }
  } while (recomputeQueued);
}

export function __resetListenersForTesting(): void {
  listeners.clear();
}
