// On-device fit engine.
//
// Given detected hardware and the signed catalog, pick the best model for each
// adaptive preset (Smallest / Half / Full) and decide which models fit (for the
// browser). All math is local: only the device knows its RAM/VRAM.
//
// Memory budget is based on TOTAL memory (stable) rather than momentary free
// memory, then scaled by the catalog's budgetFraction and reduced by a safety
// margin. Footprint = quant weights + KV cache + a fixed compute overhead +
// the vision module when the model ships one.

import {
  type AppliedModelSettings,
  type BucketRecommendation,
  type CatalogModel,
  catalogModels,
  type CatalogModelView,
  type CatalogPayload,
  type CatalogQuant,
  type CatalogVariant,
  DEFAULT_SAMPLING,
  type FitProfile,
  type HardwareInfo,
  type PresetBucket,
  primaryScore,
  type QuantOption,
  type RecommendationSet,
  type ResolvedFit,
  resolveFitProfile,
} from "@loca/shared";

const BYTES_PER_KV_ELEM = 2; // fp16 K/V cache
const MAX_THREADS = 8;
const FULL_IDLE_UNLOAD_SECONDS = 300;

// The compute overhead, the per-bucket context ceilings and the context ladder
// used to be constants here. They are catalog data now (`ResolvedFit`), because
// they are exactly the values a device measurement moves and a value that needs
// a core release to change is not one anyone retunes. Desktop resolves to the
// same numbers this file used to hardcode, which the byte-identical baseline in
// fit.test.ts is what proves. See D-158.

// Token budget for thinking, scaled to the context window. Bounding `<think>`
// guarantees the model stops reasoning and answers instead of spending the
// whole window thinking: llama-server injects the end-of-thinking tag once the
// budget is hit. Tracks the context buckets (smallest 8192 -> 512, half 16384
// -> 1024, full 32768 -> 2048).
function reasoningBudgetFor(ctx: number): number {
  if (ctx <= 8192) return 512;
  if (ctx <= 16384) return 1024;
  return 2048;
}

// Relative quality of a quant by bits-per-weight. QAT builds punch above their
// bpw, so the engine adds a bonus (see configQuantRank).
const QUANT_BPW: Record<string, number> = {
  Q8_0: 8,
  Q6_K: 6.5,
  Q5_K_M: 5.5,
  Q5_K_S: 5.3,
  Q4_K_M: 4.8,
  "UD-Q4_K_XL": 4.8,
  "UD-Q4_K_M": 4.8,
  Q4_0: 4.5,
  Q3_K_M: 3.9,
  "UD-Q3_K_M": 3.9,
  "UD-Q3_K_XL": 4.0,
  Q2_K: 2.6,
  "UD-Q2_K_XL": 2.7,
};
const QAT_QUALITY_BONUS = 1.5;

// Quality rises with bits-per-weight, but the gains above ~Q6_K are negligible
// (Q8_0 is ~+0.03% perplexity vs fp16, Q6_K ~+0.13%, Q5_K_M ~+0.39%, Q4_K_M
// ~+1.68%) while Q8_0 costs ~30% more memory than Q6_K. So the *recommended*
// quant caps here: we never suggest a quant above Q6_K when a smaller one fits.
// The community sweet spot (Q4_K_M, or Q5_K_M/Q6_K with headroom) falls under
// this ceiling; Q8_0 is offered in the picker but never recommended. Above this
// only matters as a fallback when nothing at/below it fits (very low memory).
const SWEET_SPOT_MAX_BPW = 6.5; // Q6_K

/** The single memory pool the model must fit into. Apple unified + CPU-only use
 *  system RAM; a discrete GPU targets VRAM (we offload all layers there). */
function memoryBudgetBytes(hw: HardwareInfo): number {
  if (hw.unifiedMemory) return hw.totalRamBytes;
  if (hw.gpu.backend === "cpu" || hw.gpu.vramBytes <= 0) {
    return hw.totalRamBytes;
  }
  return hw.gpu.vramBytes;
}

function gpuLayersFor(hw: HardwareInfo): number {
  // 0 = CPU only; 999 = offload all layers (Metal/CUDA/ROCm).
  return hw.gpu.backend === "cpu" ? 0 : 999;
}

function threadsFor(hw: HardwareInfo): number {
  return Math.max(1, Math.min(hw.cpuCoresPhysical || 4, MAX_THREADS));
}

function kvCacheBytes(model: CatalogModel, ctx: number): number {
  const { blockCount, headCountKv, headDim } = model.arch;
  return 2 * blockCount * ctx * headCountKv * headDim * BYTES_PER_KV_ELEM;
}

/** Zero when the profile does not preselect vision. This is one half of a pair:
 *  the other is `supportImages` in `appliedSettings`, and they must move
 *  together. Budgeting for an mmproj a profile does not preselect shrinks what
 *  fits for nothing; preselecting vision without budgeting for it overcommits
 *  memory.
 *
 *  The projector PATH is not part of that pair. It is written either way, so a
 *  profile that starts with images off is a default the user can overturn
 *  rather than a capability the build took away. */
function visionBytes(model: CatalogModel, variant: CatalogVariant, fit: ResolvedFit): number {
  if (!fit.vision) return 0;
  return model.capabilities.vision && variant.mmprojSpec ? (variant.mmprojSizeBytes ?? 0) : 0;
}

function hasVision(model: CatalogModel, variant: CatalogVariant, fit: ResolvedFit): boolean {
  return fit.vision && model.capabilities.vision && !!variant.mmprojSpec;
}

function footprintBytes(
  model: CatalogModel,
  variant: CatalogVariant,
  quant: CatalogQuant,
  ctx: number,
  fit: ResolvedFit,
): number {
  return (
    quant.fileSizeBytes +
    kvCacheBytes(model, ctx) +
    visionBytes(model, variant, fit) +
    fit.computeOverheadBytes
  );
}

function configQuantRank(variant: CatalogVariant, quant: CatalogQuant): number {
  const base = QUANT_BPW[quant.quant] ?? 4.0;
  return variant.tags.includes("qat") ? base + QAT_QUALITY_BONUS : base;
}

interface FitConfigChoice {
  model: CatalogModel;
  variant: CatalogVariant;
  quant: CatalogQuant;
  ctx: number;
  footprintBytes: number;
  quantRank: number;
}

/** Largest context (<= ceiling and the model's max) whose footprint fits the
 *  limit, or null if even the smallest context overflows. */
function bestCtx(
  model: CatalogModel,
  variant: CatalogVariant,
  quant: CatalogQuant,
  limit: number,
  ceiling: number,
  fit: ResolvedFit,
): { ctx: number; footprintBytes: number } | null {
  const cap = Math.min(ceiling, model.contextMax);
  for (const ctx of fit.ctxLadder) {
    if (ctx > cap) continue;
    const fp = footprintBytes(model, variant, quant, ctx, fit);
    if (fp <= limit) return { ctx, footprintBytes: fp };
  }
  return null;
}

/** Best (highest quality) config of a single model that fits the limit. Higher
 *  quant rank wins; ties break to the smaller footprint (which favors QAT). */
function bestConfigForModel(
  model: CatalogModel,
  limit: number,
  ceiling: number,
  cfg: ResolvedFit,
): FitConfigChoice | null {
  let best: FitConfigChoice | null = null;
  for (const variant of model.variants) {
    for (const quant of variant.quants) {
      const fit = bestCtx(model, variant, quant, limit, ceiling, cfg);
      if (!fit) continue;
      const quantRank = configQuantRank(variant, quant);
      const cand: FitConfigChoice = {
        model,
        variant,
        quant,
        ...fit,
        quantRank,
      };
      if (!best || better(cand, best)) best = cand;
    }
  }
  return best;
}

/** The quant we recommend (and apply by default) for a model on this device:
 *  the best-quality quant that fits AND sits at/below the sweet-spot ceiling
 *  (SWEET_SPOT_MAX_BPW), so we don't burn memory on Q8's diminishing returns.
 *  Falls back to a lower quant when the sweet spot doesn't fit, and to the
 *  biggest that fits only when nothing under the ceiling does (extreme low
 *  memory). Null when the model doesn't fit at all. */
function recommendedConfigForModel(
  model: CatalogModel,
  limit: number,
  ceiling: number,
  cfg: ResolvedFit,
): FitConfigChoice | null {
  let sweet: FitConfigChoice | null = null;
  let any: FitConfigChoice | null = null;
  for (const variant of model.variants) {
    for (const quant of variant.quants) {
      const fit = bestCtx(model, variant, quant, limit, ceiling, cfg);
      if (!fit) continue;
      const cand: FitConfigChoice = {
        model,
        variant,
        quant,
        ...fit,
        quantRank: configQuantRank(variant, quant),
      };
      if (!any || better(cand, any)) any = cand;
      const baseBpw = QUANT_BPW[quant.quant] ?? 4.0;
      if (baseBpw <= SWEET_SPOT_MAX_BPW && (!sweet || better(cand, sweet))) {
        sweet = cand;
      }
    }
  }
  return sweet ?? any;
}

/** The config for one specific quant: largest fitting context, or the lowest
 *  context when even that overflows (then `fits` is false). */
function configForQuant(
  model: CatalogModel,
  variant: CatalogVariant,
  quant: CatalogQuant,
  limit: number,
  cfg: ResolvedFit,
): { choice: FitConfigChoice; fits: boolean } {
  const fit = bestCtx(model, variant, quant, limit, cfg.ctxCeiling.full, cfg);
  const ctx = fit ? fit.ctx : Math.min(cfg.ctxLadder[cfg.ctxLadder.length - 1], model.contextMax);
  const fp = fit ? fit.footprintBytes : footprintBytes(model, variant, quant, ctx, cfg);
  return {
    choice: {
      model,
      variant,
      quant,
      ctx,
      footprintBytes: fp,
      quantRank: configQuantRank(variant, quant),
    },
    fits: fit !== null,
  };
}

/** Within one model: prefer higher quant quality, then smaller footprint. */
function better(a: FitConfigChoice, b: FitConfigChoice): boolean {
  if (a.quantRank !== b.quantRank) return a.quantRank > b.quantRank;
  return a.footprintBytes < b.footprintBytes;
}

/** Apply the catalog's ordered tie-breakers between two equal-score configs.
 *  Returns true if `a` should win over `b`. */
function tieBreak(a: FitConfigChoice, b: FitConfigChoice, fit: ResolvedFit): boolean {
  for (const rule of fit.tieBreakers) {
    if (rule === "lowerFootprint") {
      if (a.footprintBytes !== b.footprintBytes) {
        return a.footprintBytes < b.footprintBytes;
      }
    } else if (rule.startsWith("preferTag:")) {
      const tag = rule.slice("preferTag:".length);
      const at = a.variant.tags.includes(tag);
      const bt = b.variant.tags.includes(tag);
      if (at !== bt) return at;
    }
  }
  return false;
}

function appliedSettings(
  c: FitConfigChoice,
  hw: HardwareInfo,
  idleUnloadSeconds: number,
  fit: ResolvedFit,
): AppliedModelSettings {
  const vision = hasVision(c.model, c.variant, fit);
  // The model's author-recommended sampling, or the universal default. Written
  // to the llm.* sampling settings on select so each model gets its tuning.
  const sampling = c.model.sampling ?? DEFAULT_SAMPLING;
  return {
    modelPath: c.quant.modelSpec,
    // The projector path is a fact about the MODEL, so it is written on every
    // profile, including one that does not preselect or budget for vision. A
    // profile skips the flag and the footprint; erasing the path too would
    // leave `llm.supportImages` a switch wired to nothing, which is the product
    // deciding for the user rather than defaulting for them.
    mmprojPath: c.model.capabilities.vision ? c.variant.mmprojSpec : undefined,
    contextSize: c.ctx,
    threads: threadsFor(hw),
    gpuLayers: gpuLayersFor(hw),
    flashAttn: true,
    supportImages: vision,
    idleUnloadSeconds,
    reasoningBudget: reasoningBudgetFor(c.ctx),
    temperature: sampling.temperature,
    topP: sampling.topP,
    topK: sampling.topK,
    minP: sampling.minP,
    repeatPenalty: sampling.repeatPenalty,
    dryMultiplier: sampling.dryMultiplier ?? DEFAULT_SAMPLING.dryMultiplier,
    presencePenalty: sampling.presencePenalty ?? DEFAULT_SAMPLING.presencePenalty,
    // Only catalog models carry a sampler order; others keep the server default.
    samplers: sampling.samplers ?? DEFAULT_SAMPLING.samplers,
  };
}

function toRecommendation(
  bucket: PresetBucket,
  c: FitConfigChoice,
  hw: HardwareInfo,
  fit: ResolvedFit,
): BucketRecommendation {
  const idle = bucket === "full" ? FULL_IDLE_UNLOAD_SECONDS : 0;
  return {
    bucket,
    modelId: c.model.id,
    name: c.model.name,
    quality: primaryScore(c.model, fit.primaryScore) ?? 0,
    quant: c.quant.quant,
    footprintBytes: c.footprintBytes,
    vision: hasVision(c.model, c.variant, fit),
    variantLabel: c.variant.label,
    apply: appliedSettings(c, hw, idle, fit),
  };
}

/** Pick the highest-scoring model whose config (chosen by `configFor`) fits.
 *  `configFor` decides the quant policy per bucket (sweet-spot vs biggest). */
function pickBestModel(
  models: CatalogModel[],
  fit: ResolvedFit,
  configFor: (model: CatalogModel) => FitConfigChoice | null,
): FitConfigChoice | null {
  let winner: FitConfigChoice | null = null;
  let winnerScore = -Infinity;
  for (const model of models) {
    const score = primaryScore(model, fit.primaryScore);
    if (score === undefined) continue;
    const config = configFor(model);
    if (!config) continue;
    if (
      score > winnerScore ||
      (score === winnerScore && winner !== null && tieBreak(config, winner, fit))
    ) {
      winner = config;
      winnerScore = score;
    }
  }
  return winner;
}

/** Lowest-footprint model that clears the quality floor and fits (Smallest).
 *  `configFor` decides the quant policy (Smallest uses the recommended quant). */
function pickSmallest(
  models: CatalogModel[],
  fit: ResolvedFit,
  floorScore: number,
  configFor: (model: CatalogModel) => FitConfigChoice | null,
): FitConfigChoice | null {
  let winner: FitConfigChoice | null = null;
  for (const model of models) {
    const score = primaryScore(model, fit.primaryScore);
    if (score === undefined || score < floorScore) continue;
    const config = configFor(model);
    if (!config) continue;
    if (!winner || config.footprintBytes < winner.footprintBytes) {
      winner = config;
    }
  }
  return winner;
}

function floorScore(catalog: CatalogPayload, fit: ResolvedFit): number {
  const ref = catalogModels(catalog).find((m) => m.id === fit.smallestQualityFloorRef);
  return ref ? (primaryScore(ref, fit.primaryScore) ?? 0) : 0;
}

/** Compute the three adaptive presets for this device.
 *
 *  `profile` selects which set of tuning the catalog's policy resolves to. It is
 *  an explicit argument rather than something derived inside, so a test can drive
 *  both on one machine: a `detect`-shaped function whose every caller passes the
 *  dominant value has an unobserved parameter, which is D-145 exactly. The
 *  derivation lives at the route, as `fitProfileForOs(hw.os)`. */
export function computeRecommendations(
  catalog: CatalogPayload,
  hw: HardwareInfo,
  applied: { preset?: string; modelPath?: string },
  profile: FitProfile = "desktop",
): RecommendationSet {
  const fit = resolveFitProfile(catalog.fit, profile);
  const models = catalogModels(catalog);
  const budget = memoryBudgetBytes(hw);
  const margin = fit.safetyMarginBytes;
  const halfLimit = budget * fit.budgetFraction.half - margin;
  const fullLimit = budget * fit.budgetFraction.full - margin;

  // Smallest + Balanced apply the recommended (sweet-spot) quant; Balanced
  // steps down to a lower quant if the sweet spot doesn't fit its budget (that
  // fallback is built into recommendedConfigForModel). Full maxes out: the
  // biggest quant that fits.
  const smallest = pickSmallest(models, fit, floorScore(catalog, fit), (m) =>
    recommendedConfigForModel(m, fullLimit, fit.ctxCeiling.smallest, fit),
  );
  const half = pickBestModel(models, fit, (m) =>
    recommendedConfigForModel(m, halfLimit, fit.ctxCeiling.half, fit),
  );
  const full = pickBestModel(models, fit, (m) =>
    bestConfigForModel(m, fullLimit, fit.ctxCeiling.full, fit),
  );

  return {
    hardware: hw,
    catalogGeneratedAt: catalog.generatedAt,
    buckets: {
      smallest: smallest ? toRecommendation("smallest", smallest, hw, fit) : null,
      half: half ? toRecommendation("half", half, hw, fit) : null,
      full: full ? toRecommendation("full", full, hw, fit) : null,
    },
    applied,
  };
}

/** One browser row per catalog model: the quant we recommend (and apply by
 *  default) for this device, plus every quant the user can pick manually. When
 *  nothing fits, falls back to the smallest config and reports fits=false. */
export function buildCatalogViews(
  catalog: CatalogPayload,
  hw: HardwareInfo,
  profile: FitProfile = "desktop",
): CatalogModelView[] {
  const fit = resolveFitProfile(catalog.fit, profile);
  const budget = memoryBudgetBytes(hw);
  const fullLimit = budget * fit.budgetFraction.full - fit.safetyMarginBytes;

  return catalogModels(catalog).map((model) => {
    const fitting = recommendedConfigForModel(model, fullLimit, fit.ctxCeiling.full, fit);
    const config = fitting ?? smallestConfig(model, fit);
    const quants: QuantOption[] = [];
    for (const variant of model.variants) {
      for (const quant of variant.quants) {
        const { choice, fits } = configForQuant(model, variant, quant, fullLimit, fit);
        quants.push({
          modelSpec: quant.modelSpec,
          quant: quant.quant,
          variantLabel: variant.label,
          footprintBytes: choice.footprintBytes,
          fits,
          recommended: quant.modelSpec === config.quant.modelSpec,
        });
      }
    }
    const view: CatalogModelView = {
      id: model.id,
      family: model.family,
      name: model.name,
      quality: primaryScore(model, fit.primaryScore),
      paramsB: model.paramsB,
      vision: fit.vision && model.capabilities.vision,
      tags: [...new Set(model.variants.flatMap((v) => v.tags))],
      quant: config.quant.quant,
      variantLabel: config.variant.label,
      footprintBytes: config.footprintBytes,
      fits: fitting !== null,
      apply: appliedSettings(config, hw, 0, fit),
      quants,
    };
    return view;
  });
}

/** Resolve apply-settings for a specific quant by its unique modelSpec. Used by
 *  the manual quantization picker. Returns null if the spec is not in the
 *  catalog. */
export function appliedForModelSpec(
  catalog: CatalogPayload,
  hw: HardwareInfo,
  modelSpec: string,
  profile: FitProfile = "desktop",
): { modelId: string; apply: AppliedModelSettings } | null {
  const fit = resolveFitProfile(catalog.fit, profile);
  const budget = memoryBudgetBytes(hw);
  const fullLimit = budget * fit.budgetFraction.full - fit.safetyMarginBytes;
  for (const model of catalogModels(catalog)) {
    for (const variant of model.variants) {
      for (const quant of variant.quants) {
        if (quant.modelSpec !== modelSpec) continue;
        const { choice } = configForQuant(model, variant, quant, fullLimit, fit);
        return { modelId: model.id, apply: appliedSettings(choice, hw, 0, fit) };
      }
    }
  }
  return null;
}

/** Cheapest config of a model (used for the browser row when nothing fits): the
 *  smallest-footprint quant at the lowest context. */
function smallestConfig(model: CatalogModel, fit: ResolvedFit): FitConfigChoice {
  let best: FitConfigChoice | null = null;
  const ctx = Math.min(fit.ctxLadder[fit.ctxLadder.length - 1], model.contextMax);
  for (const variant of model.variants) {
    for (const quant of variant.quants) {
      const fp = footprintBytes(model, variant, quant, ctx, fit);
      if (!best || fp < best.footprintBytes) {
        best = {
          model,
          variant,
          quant,
          ctx,
          footprintBytes: fp,
          quantRank: configQuantRank(variant, quant),
        };
      }
    }
  }
  // Every model has >= 1 variant with >= 1 quant (schema-enforced).
  return best as FitConfigChoice;
}
