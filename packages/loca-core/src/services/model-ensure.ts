// Resolve which model files each sidecar kind needs from the current
// settings, then enqueue downloads for any that aren't on disk yet.
//
// Note: ensureKindModels enqueues real (multi-GB) downloads, so it should only
// run from a user-confirmed path: POST /api/v1/models/ensure (an explicit
// download button) or POST /api/v1/requirements/download (the Pending Downloads
// confirm modal). It is deliberately not called on a settings change or a
// preset/model select; those just write settings, which prompts the client's
// confirm modal via the requirements snapshot, and the user starts the download
// from there.

import { requiredModelRefs } from "@loca/shared";
import type { RequirementGroup } from "@loca/shared";
import { modelsManager } from "../models/manager.ts";
import { loadCoreSettingsResolved } from "@loca/core-engine/services/core-settings";

export type ModelKind = RequirementGroup;

export interface EnsureResult {
  enqueued: string[]; // download queue-row ids (accepted by cancel/retry/remove)
  alreadyHave: string[];
}

/** HF source specs the named sidecar kind needs given the current settings.
 *  Delegates to the shared `requiredModelRefs` mapping (the single source of
 *  truth shared with the client). Returns [] when the kind is disabled / on an
 *  external provider. */
export async function sourcesForKind(kind: ModelKind): Promise<string[]> {
  const settings = await loadCoreSettingsResolved();
  return requiredModelRefs(settings)
    .filter((r) => r.group === kind)
    .map((r) => r.source);
}

/** Probe + enqueue any missing files for the kind. Idempotent: files already
 *  present are reported in `alreadyHave`. */
export async function ensureKindModels(kind: ModelKind): Promise<EnsureResult> {
  const sources = await sourcesForKind(kind);
  if (sources.length === 0) return { enqueued: [], alreadyHave: [] };
  const probes = await modelsManager().probe(sources);
  const alreadyHave = probes.filter((p) => p.alreadyHave).map((p) => p.source);
  const missing = probes.filter((p) => !p.alreadyHave).map((p) => p.source);
  if (missing.length === 0) return { enqueued: [], alreadyHave };
  const enqueued = modelsManager().download(missing.map((source) => ({ source, group: kind })));
  return { enqueued, alreadyHave };
}
