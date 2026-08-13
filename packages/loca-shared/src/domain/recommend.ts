// Hardware introspection + per-device model recommendations.
//
// HardwareInfo is produced by the `loca-core-hwinfo` helper binary and consumed
// by core's fit engine. BucketRecommendation/RecommendationSet are the wire shape
// of GET /api/v1/models/recommend; CatalogModelView is one row of
// GET /api/v1/models/catalog (the model browser). The fit math itself lives in
// core (packages/loca-core/src/models/fit.ts); these are just the shared shapes.

import type { SttFamily, TtsFamily } from "./catalog.ts";

// `vulkan` is a generic GPU backend (any Vulkan-capable device: Intel, and
// NVIDIA/AMD where no vendor-specific build applies). It drives GPU-layer
// offload like the vendor backends, and selects the `vulkan` binary variant.
export type GpuBackend = "metal" | "cuda" | "rocm" | "vulkan" | "cpu";

export interface GpuInfo {
  backend: GpuBackend;
  name: string;
  /** Dedicated VRAM in bytes. 0 on CPU-only or when unified (see
   *  `unifiedMemory`), where system RAM is the budget. */
  vramBytes: number;
}

export interface HardwareInfo {
  totalRamBytes: number;
  availableRamBytes: number;
  cpuCoresPhysical: number;
  gpu: GpuInfo;
  /** True on Apple Silicon: GPU shares system RAM, so the memory budget is
   *  availableRamBytes rather than a separate VRAM pool. */
  unifiedMemory: boolean;
}

/** The three adaptive presets. `custom` (a manual model pick or hand-edited
 *  fields) is not a computed bucket and is absent here. */
export type PresetBucket = "smallest" | "half" | "full";

export const PRESET_BUCKETS: readonly PresetBucket[] = ["smallest", "half", "full"] as const;

/** The sampling knobs a model runs with at inference time. The catalog carries
 *  an optional per-model `sampling` tuned by its authors; DEFAULT_SAMPLING fills
 *  in for anything without one (custom local models, external providers). */
export interface Sampling {
  temperature: number;
  topP: number;
  topK: number;
  minP: number;
  /** llama.cpp repetition penalty; 1.0 = disabled. */
  repeatPenalty: number;
  /** DRY (Don't Repeat Yourself) multiplier; 0 = disabled. Penalizes repeating
   *  whole phrases, catching the semantic loops the token-level repeat penalty
   *  misses. Omitted by a model's catalog tuning falls back to DEFAULT_SAMPLING. */
  dryMultiplier?: number;
  /** OpenAI-style presence penalty; 0 = disabled. Nudges the model toward new
   *  words instead of reusing ones it already produced. */
  presencePenalty?: number;
  /** llama.cpp sampler-chain order (canonical names, applied in this sequence).
   *  Omitted = the server's default chain. Set only on catalog models known to
   *  benefit; see LOOP_RESISTANT_SAMPLER_ORDER. */
  samplers?: string[];
}

/** A sampler chain that runs the truncation samplers and temperature BEFORE the
 *  repetition guards (DRY, penalties), rather than llama.cpp's default of
 *  penalties-first. On some quantized reasoning models the default order lets
 *  the penalties bias sampling into a loop; deferring them until after
 *  temperature avoids that. Opt-in per catalog model, never a global default. */
export const LOOP_RESISTANT_SAMPLER_ORDER: readonly string[] = [
  "top_k",
  "typ_p",
  "top_p",
  "min_p",
  "xtc",
  "temperature",
  "dry",
  "penalties",
];

/** Fallback sampling for a model with no catalog-specific values, and the
 *  default shown for the `llm.*` sampling fields. Conservative values that suit
 *  a small instruct model. */
export const DEFAULT_SAMPLING: Sampling = {
  temperature: 0.7,
  topP: 0.8,
  topK: 20,
  minP: 0,
  repeatPenalty: 1.05,
  // A moderate DRY multiplier on by default: small local models loop without it,
  // and 0.8 is gentle enough to leave normal prose untouched. 0 turns it off.
  dryMultiplier: 0.8,
  presencePenalty: 0,
};

/** The concrete `llm.*` values a recommendation or a manual pick applies. Mirrors
 *  the LlamaStartArgs-relevant settings the core writes on select. */
export interface AppliedModelSettings extends Sampling {
  modelPath: string; // HF spec "@provider/repo/branch/file.gguf"
  mmprojPath?: string;
  contextSize: number;
  threads: number;
  gpuLayers: number;
  flashAttn: boolean;
  /** Side effect of the chosen model: true iff it ships a vision module. */
  supportImages: boolean;
  /** > 0 only for the Full bucket (aggressive idle-unload). */
  idleUnloadSeconds: number;
  /** Token budget for the model's thinking, scaled to the context window. Caps
   *  the `<think>` block so a turn always leaves room to reach its answer. */
  reasoningBudget: number;
}

export interface BucketRecommendation {
  bucket: PresetBucket;
  modelId: string;
  name: string;
  /** Resolved primary "smartness" score. */
  quality: number;
  quant: string;
  footprintBytes: number;
  vision: boolean;
  variantLabel: string;
  apply: AppliedModelSettings;
}

export interface RecommendationSet {
  hardware: HardwareInfo;
  catalogGeneratedAt: string;
  /** null for a bucket when nothing in the catalog fits (extreme low memory). */
  buckets: Record<PresetBucket, BucketRecommendation | null>;
  /** What the live settings currently point at, so the client can diff. */
  applied: { preset?: string; modelPath?: string };
}

/** The concrete `stt.*` values a Speech-to-Text selection applies (host/port
 *  are never touched). `modelFiles` is the JSON role->spec bundle the sidecar
 *  and download flow read; `modelPath` is the primary file's spec (the identity
 *  used to match a custom pick and show which model is selected). */
export interface AppliedSttSettings {
  modelType: SttFamily;
  modelPath: string; // primary file spec "@org/repo/branch/file.onnx"
  modelFiles: string; // JSON `{ role: spec }`
  threads: number;
}

/** One curated Speech-to-Text card resolved against the catalog, the wire shape
 *  of GET /api/v1/models/stt/catalog presets (badges for the client). */
export interface SttPresetView {
  id: string;
  modelId: string;
  name: string;
  family: SttFamily;
  english: boolean;
  quant: string;
  modelSpec: string; // primary file spec (identity)
  fileSizeBytes: number;
}

/** The concrete `tts.*` values a Text-to-Speech selection applies. Mirrors
 *  AppliedSttSettings; TTS is local-only and single-threaded so no `threads`. */
export interface AppliedTtsSettings {
  modelType: TtsFamily;
  modelPath: string; // primary file spec
  modelFiles: string; // JSON `{ role: spec }`
  voice: string; // the model's default voice id
}

/** One curated Text-to-Speech card resolved against the catalog (badges for the
 *  client). Carries the model's voices so the voice dropdown can populate from
 *  the selection without a second round-trip. */
export interface TtsPresetView {
  id: string;
  modelId: string;
  name: string;
  family: TtsFamily;
  quant: string;
  modelSpec: string; // primary file spec (identity)
  fileSizeBytes: number;
  voices: { id: string; label: string; lang?: string }[];
  defaultVoice: string;
}

/** One selectable quantization of a model, with how it lands on this device. */
export interface QuantOption {
  /** Unique HF spec "@provider/repo/branch/file.gguf" (identifies the quant). */
  modelSpec: string;
  quant: string;
  variantLabel: string;
  footprintBytes: number;
  fits: boolean;
  /** The quality/size sweet spot the fit engine recommends (and applies by
   *  default when this model is picked). Capped below Q8's diminishing returns. */
  recommended: boolean;
}

/** One row in the model browser: a catalog model plus how it would land on this
 *  device. */
export interface CatalogModelView {
  id: string;
  family: string;
  name: string;
  quality?: number;
  paramsB: number;
  vision: boolean;
  tags: string[];
  /** Best quant/variant that fits this device, or the smallest quant if none
   *  fit (then `fits` is false). */
  quant: string;
  variantLabel: string;
  footprintBytes: number;
  fits: boolean;
  apply: AppliedModelSettings;
  /** Every quant of this model the user can pick manually (recommended flagged). */
  quants: QuantOption[];
}
