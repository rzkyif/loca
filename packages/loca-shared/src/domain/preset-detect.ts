/**
 * Detects which llm.* preset a settings snapshot corresponds to. The settings
 * UI flips `llm.preset` to Custom whenever a managed field is edited; this lets
 * it be smarter and restore a smart preset when the values line back up with
 * one.
 *
 * Returns a bucket id ("smallest" / "half" / "full") when the managed fields
 * match that bucket's applied config exactly, otherwise "custom". The UI then
 * distinguishes a recognized catalog model ("custom preset") from a fully
 * hand-configured one ("custom -> manual") by whether `llm.modelPath` resolves
 * to a catalog quant, so that split doesn't need a separate stored value here.
 */

import { type AppliedModelSettings, PRESET_BUCKETS, type PresetBucket } from "./recommend.ts";

export const CUSTOM_PRESET = "custom";

/** Managed llm.* keys paired with their field on AppliedModelSettings. Mirrors
 *  `llm.preset`'s managedKeys (the model identity + tuning), minus the behavior
 *  prefs (temperature, reasoningBudget) and Image Attachments (a user choice,
 *  not part of the model identity) that no longer define a preset. */
const MANAGED_FIELDS: ReadonlyArray<[string, keyof AppliedModelSettings]> = [
  ["llm.modelPath", "modelPath"],
  ["llm.mmprojPath", "mmprojPath"],
  ["llm.contextSize", "contextSize"],
  ["llm.threads", "threads"],
  ["llm.gpuLayers", "gpuLayers"],
  ["llm.flashAttn", "flashAttn"],
  ["llm.idleUnloadSeconds", "idleUnloadSeconds"],
  ["llm.topP", "topP"],
  ["llm.topK", "topK"],
  ["llm.minP", "minP"],
  ["llm.repeatPenalty", "repeatPenalty"],
];

export function detectLlmPreset(
  settings: Record<string, unknown>,
  buckets: Partial<Record<PresetBucket, AppliedModelSettings | null | undefined>>,
): string {
  for (const bucket of PRESET_BUCKETS) {
    const apply = buckets[bucket];
    if (apply && appliedMatches(settings, apply)) return bucket;
  }
  return CUSTOM_PRESET;
}

function appliedMatches(settings: Record<string, unknown>, apply: AppliedModelSettings): boolean {
  for (const [key, field] of MANAGED_FIELDS) {
    if (!valueEquals(settings[key], apply[field])) return false;
  }
  return true;
}

function valueEquals(got: unknown, want: unknown): boolean {
  if (typeof want === "number") {
    const n =
      typeof got === "number" ? got : typeof got === "string" && got !== "" ? Number(got) : NaN;
    return n === want;
  }
  if (typeof want === "boolean") return got === want;
  // An absent/empty applied value (e.g. no vision module) matches an unset key.
  if (want === undefined || want === "") {
    return got === undefined || got === null || got === "";
  }
  return got === want;
}
