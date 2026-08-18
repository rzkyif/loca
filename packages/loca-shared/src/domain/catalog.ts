// Model catalog: the hand-authored, signed list of local models Loca can run,
// served at get.loca.rzkyif.com/manifests[/<channel>]/catalog.json.
//
// Authoring lives in the `@loca/model-catalog` package as one TS file per model
// family; `scripts/release/catalog.ts` validates each family against the schemas
// here, flattens them into one ModelCatalog, Ed25519-signs it (whole object minus
// `signature`, canonicalized; same trust root as core.json/binaries.json), and
// uploads it. The core fetches + verifies it and runs the fit engine on-device.
//
// Deliberately provider-agnostic: the GGUF provider is whatever the `@provider/...`
// modelSpec encodes, scores are a generic provider-tagged list, and the
// "smartness" metric + tie-break policy are data-driven in FitConfig. Unsloth
// (GGUF) and Artificial Analysis (score) are the current sources, not baked in.

import { z } from "zod";

/** One capability score from one provider, e.g. Artificial Analysis's
 *  intelligence index. Kept generic so a different provider/metric can be added
 *  without a schema change; FitConfig.primaryScore selects which one ranks. */
export const modelScoreSchema = z
  .object({
    source: z.string(), // e.g. "artificial-analysis"
    metric: z.string(), // e.g. "intelligence-index"
    value: z.number(),
  })
  .strict();
export type ModelScore = z.infer<typeof modelScoreSchema>;

/** A single GGUF quantization. Weights size only; the architecture fields used
 *  for KV-cache math live on the model (identical across quants). */
export const catalogQuantSchema = z
  .object({
    quant: z.string(), // "Q4_K_M", "Q8_0", ...
    modelSpec: z.string(), // "@unsloth/repo/branch/file.gguf"
    fileSizeBytes: z.number().int().positive(),
    // Pinned content sha256 (git-LFS oid). The download path verifies the bytes
    // against this signed value instead of HF's x-linked-etag. Stamped by
    // buildCatalogPayload from hashes.generated.ts; absent only for a file whose
    // hash hasn't been gathered yet (then the download falls back to HF's hash).
    sha256: z
      .string()
      .regex(/^[0-9a-f]{64}$/)
      .optional(),
  })
  .strict();
export type CatalogQuant = z.infer<typeof catalogQuantSchema>;

/** One file of a multi-file speech (STT/TTS) bundle, tagged by the role the
 *  sherpa-onnx config struct expects it under, e.g. "encoder", "decoder",
 *  "joiner", "tokens", "model", "voices", "vocoder". The sidecar maps roles to
 *  the active family's config fields. */
export const catalogFileRoleSchema = z
  .object({
    role: z.string(), // "encoder" | "decoder" | "tokens" | "model" | "voices" | ...
    modelSpec: z.string(), // "@user/repo/branch/file"
    fileSizeBytes: z.number().int().positive(),
    // Pinned content sha256; see catalogQuantSchema.sha256.
    sha256: z
      .string()
      .regex(/^[0-9a-f]{64}$/)
      .optional(),
  })
  .strict();
export type CatalogFileRole = z.infer<typeof catalogFileRoleSchema>;

/** A single quantization of a speech model. Unlike GGUF (one file), a speech
 *  model is a bundle of role-tagged files; `fileSizeBytes` is their sum, shown
 *  on the preset/model card. */
export const speechQuantSchema = z
  .object({
    quant: z.string(), // "int8", "fp16", ...
    files: z.array(catalogFileRoleSchema).min(1),
    fileSizeBytes: z.number().int().positive(), // summed bundle size for the badge
  })
  .strict();
export type SpeechQuant = z.infer<typeof speechQuantSchema>;

/** Transformer shape needed to estimate the KV-cache footprint without reading
 *  the GGUF. headDim is explicit because it is not always embeddingLength /
 *  headCount (e.g. Gemma uses a larger head_dim). */
export const modelArchSchema = z
  .object({
    blockCount: z.number().int().positive(), // n_layers
    embeddingLength: z.number().int().positive(), // n_embd
    headCount: z.number().int().positive(), // n_head
    headCountKv: z.number().int().positive(), // n_head_kv (GQA)
    headDim: z.number().int().positive(), // key/value length per head
  })
  .strict();
export type ModelArch = z.infer<typeof modelArchSchema>;

/** A distribution of one model: standard weights, a QAT build, etc. Variants of
 *  one model share its scores; they differ in memory footprint. `tags` is
 *  open-ended so the fit engine's tie-breakers can prefer e.g. "qat". */
export const catalogVariantSchema = z
  .object({
    label: z.string(), // "standard", "QAT", ...
    tags: z.array(z.string()).default([]), // ["qat"], ["moe"], ...
    provider: z.string(), // "unsloth" (also implied by modelSpec)
    mmprojSpec: z.string().optional(), // vision module GGUF, if any
    mmprojSizeBytes: z.number().int().positive().optional(),
    mmprojSha256: z
      .string()
      .regex(/^[0-9a-f]{64}$/)
      .optional(), // pinned, see catalogQuantSchema.sha256
    quants: z.array(catalogQuantSchema).min(1), // best quality first
  })
  .strict();
export type CatalogVariant = z.infer<typeof catalogVariantSchema>;

/** Author-recommended sampling for a model, applied to the `llm.*` sampling
 *  settings when a preset resolves to it. Optional: a model without it falls
 *  back to DEFAULT_SAMPLING (see recommend.ts). */
export const samplingSchema = z
  .object({
    temperature: z.number().nonnegative(),
    topP: z.number().min(0).max(1),
    topK: z.number().int().nonnegative(),
    minP: z.number().min(0).max(1),
    repeatPenalty: z.number().positive(), // 1.0 = disabled
    dryMultiplier: z.number().nonnegative().optional(), // 0 = disabled
    presencePenalty: z.number().optional(), // 0 = disabled
    samplers: z.array(z.string()).optional(), // llama.cpp sampler-chain order
  })
  .strict();
export type CatalogSampling = z.infer<typeof samplingSchema>;

export const catalogModelSchema = z
  .object({
    id: z.string(), // canonical HF base id, e.g. "Qwen/Qwen3.5-4B"
    name: z.string(), // "Qwen3.5 4B"
    scores: z.array(modelScoreSchema).default([]),
    paramsB: z.number().positive(),
    contextMax: z.number().int().positive(),
    arch: modelArchSchema,
    capabilities: z
      .object({
        tools: z.boolean(),
        vision: z.boolean(),
        reasoning: z.boolean(),
      })
      .strict(),
    sampling: samplingSchema.optional(),
    variants: z.array(catalogVariantSchema).min(1),
  })
  .strict();
export type CatalogModel = z.infer<typeof catalogModelSchema>;

/** One authoring file in `@loca/model-catalog`. */
export const modelFamilySchema = z
  .object({
    family: z.string(), // "Qwen 3.5"
    publisher: z.string(), // "Qwen"
    models: z.array(catalogModelSchema).min(1),
  })
  .strict();
export type ModelFamily = z.infer<typeof modelFamilySchema>;

/** Which sherpa-onnx offline recognizer config a Speech-to-Text model loads
 *  under. The sidecar (and its Rust `SttConfig` enum) dispatch on this.
 *
 *  The tuple comes first and the schema is derived from it, rather than the
 *  `z.infer` shape the rest of this module uses. `domain/recommend.ts` names
 *  this type, and a module reachable only through a `z.infer` alias cannot be
 *  read by the TypeScript half of the contract generator at all: it falls over
 *  resolving zod's internals. One list still feeds both, so the two cannot
 *  drift. */
export const STT_FAMILIES = [
  "whisper",
  "sense-voice",
  "moonshine",
  "paraformer",
  "transducer",
  "nemo-ctc",
  "dolphin",
  "telespeech-ctc",
] as const;
export type SttFamily = (typeof STT_FAMILIES)[number];
export const sttFamilySchema = z.enum(STT_FAMILIES);

/** One Speech-to-Text model. Flat: no variants and no fit math. `family` picks
 *  the sherpa-onnx config struct; `english` is the load-bearing axis for cards
 *  (an `.en` bundle transcribes English slightly better than its multilingual
 *  sibling of the same size). */
export const sttCatalogModelSchema = z
  .object({
    id: z.string(), // "whisper-small.en"
    name: z.string(), // "Whisper Small (English)"
    family: sttFamilySchema,
    english: z.boolean(), // true = English-only; false = multilingual
    quants: z.array(speechQuantSchema).min(1), // best quality first
  })
  .strict();
export type SttCatalogModel = z.infer<typeof sttCatalogModelSchema>;

/** Binds one curated Speech-to-Text card (an stt.preset option id) to a
 *  model+quant, so a catalog re-release can repoint a card without a core or
 *  client release. Card copy lives in the settings schema. */
export const sttPresetSchema = z
  .object({
    id: z.string(), // "light-multilingual"
    modelId: z.string(),
    quant: z.string(),
  })
  .strict();
export type SttPreset = z.infer<typeof sttPresetSchema>;

export const sttCatalogSchema = z
  .object({
    models: z.array(sttCatalogModelSchema).min(1),
    presets: z.array(sttPresetSchema).min(1),
  })
  .strict();
export type SttCatalog = z.infer<typeof sttCatalogSchema>;

/** Which sherpa-onnx offline TTS config a Text-to-Speech model loads under.
 *  Tuple-first for the same reason as {@link STT_FAMILIES}. */
export const TTS_FAMILIES = ["kokoro", "kitten", "vits", "matcha"] as const;
export type TtsFamily = (typeof TTS_FAMILIES)[number];
export const ttsFamilySchema = z.enum(TTS_FAMILIES);

/** One selectable voice of a TTS model. `id` is what the sidecar maps to a
 *  speaker id for the active family (Kokoro: an ordered name; single-speaker
 *  families: "0"). `lang` is the espeak language a Kokoro/Kitten voice reloads
 *  in, when relevant. */
export const ttsVoiceSchema = z
  .object({
    id: z.string(), // "af_bella", "0", ...
    label: z.string(), // "American - Female - Bella"
    lang: z.string().optional(), // espeak code, for cross-language reload
  })
  .strict();
export type TtsVoice = z.infer<typeof ttsVoiceSchema>;

/** One Text-to-Speech model. `family` picks the sherpa-onnx config struct;
 *  `voices` is the model's own speaker list (Kokoro 53, Kitten 8, single-speaker
 *  families one). */
export const ttsCatalogModelSchema = z
  .object({
    id: z.string(), // "kokoro-multi"
    name: z.string(), // "Kokoro Multilingual"
    family: ttsFamilySchema,
    voices: z.array(ttsVoiceSchema).min(1),
    defaultVoice: z.string(),
    quants: z.array(speechQuantSchema).min(1), // best quality first
  })
  .strict();
export type TtsCatalogModel = z.infer<typeof ttsCatalogModelSchema>;

/** Binds one curated Text-to-Speech card (a tts.preset option id) to a
 *  model+quant. Mirrors sttPresetSchema. */
export const ttsPresetSchema = z
  .object({
    id: z.string(), // "kokoro-default"
    modelId: z.string(),
    quant: z.string(),
  })
  .strict();
export type TtsPreset = z.infer<typeof ttsPresetSchema>;

export const ttsCatalogSchema = z
  .object({
    models: z.array(ttsCatalogModelSchema).min(1),
    presets: z.array(ttsPresetSchema).min(1),
  })
  .strict();
export type TtsCatalog = z.infer<typeof ttsCatalogSchema>;

/** The file role that identifies each family's "main" weights file, used as the
 *  modelPath identity (which model is selected) and to match a custom pick. */
export const STT_PRIMARY_ROLE: Record<SttFamily, string> = {
  whisper: "encoder",
  "sense-voice": "model",
  moonshine: "encoder",
  paraformer: "model",
  transducer: "encoder",
  "nemo-ctc": "model",
  dolphin: "model",
  "telespeech-ctc": "model",
};
export const TTS_PRIMARY_ROLE: Record<TtsFamily, string> = {
  kokoro: "model",
  kitten: "model",
  vits: "model",
  matcha: "acoustic_model",
};

/** The role->spec object a `*.modelFiles` setting stores, from a quant's files. */
export function modelFilesObject(quant: SpeechQuant): Record<string, string> {
  const out: Record<string, string> = {};
  for (const f of quant.files) out[f.role] = f.modelSpec;
  return out;
}

/** The primary (identity) file spec of a quant for the given primary role,
 *  falling back to the first file so a caller always gets a spec. */
export function primaryFileSpec(quant: SpeechQuant, primaryRole: string): string {
  return quant.files.find((f) => f.role === primaryRole)?.modelSpec ?? quant.files[0].modelSpec;
}

/** Selects which ModelScore ranks "smartness". Swapping the score provider is a
 *  data change here, not a code change. */
export const scoreSelectorSchema = z
  .object({
    source: z.string(),
    metric: z.string(),
  })
  .strict();
export type ScoreSelector = z.infer<typeof scoreSelectorSchema>;

const budgetFractionSchema = z
  .object({
    half: z.number().positive().max(1),
    full: z.number().positive().max(1),
  })
  .strict();

const ctxCeilingSchema = z
  .object({
    smallest: z.number().int().positive(),
    half: z.number().int().positive(),
    full: z.number().int().positive(),
  })
  .strict();

/** Today's desktop values, named so the schema defaults and any reader agree by
 *  construction rather than by two people typing the same number. A catalog
 *  published before these fields existed parses to exactly these, which is what
 *  makes the fields additive rather than a behaviour change. */
export const DESKTOP_COMPUTE_OVERHEAD_BYTES = 600_000_000;
export const DESKTOP_CTX_CEILING = { smallest: 8192, half: 16384, full: 32768 } as const;
export const DESKTOP_CTX_LADDER = [32768, 16384, 8192, 4096] as const;

/** The context ladder, strictly descending.
 *
 *  **The order is load-bearing, and until F-A it was unenforced.** `bestCtx`
 *  returns the FIRST rung that fits, and two other sites read the LAST element
 *  as "the lowest rung", so an ascending ladder silently changes what the engine
 *  answers. The Rust port normalised its copy and the TypeScript did not, so one
 *  signed catalog produced ctx 2048 on one core and 4096 on the other, with the
 *  Rust suite entirely green. This is the exact field the mobile profile makes
 *  data so S0.4 can retune it, which means the first person to write it in
 *  natural reading order is the person who finds the divergence.
 *
 *  Bounded HERE, in the schema both cores generate from, rather than fixed in
 *  one engine: a bound only one side enforces is the divergence again. Both
 *  resolvers ALSO normalise, so a hand-built config that never met the schema
 *  still gets the same answer from both. */
const descendingLadder = z
  .array(z.number().int().positive())
  .min(1)
  .refine((l) => l.every((n, i) => i === 0 || n < l[i - 1]), {
    message: "ctxLadder must be strictly descending: the fit engine takes the first rung that fits",
  });

/** Per-profile overrides. Every field is optional and means "leave the base
 *  value alone", so a profile states only what differs.
 *
 *  Why these particular fields are data rather than code: they are the ones a
 *  device measurement moves. The mobile budget fractions and the compute
 *  overhead can only be set by measuring a real phone, and a value that needs a
 *  core release to change is not a value anyone will retune. See D-158. */
export const fitProfileOverridesSchema = z
  .object({
    budgetFraction: budgetFractionSchema.optional(),
    safetyMarginBytes: z.number().int().nonnegative().optional(),
    smallestQualityFloorRef: z.string().optional(),
    computeOverheadBytes: z.number().int().nonnegative().optional(),
    ctxCeiling: ctxCeilingSchema.optional(),
    ctxLadder: descendingLadder.optional(),
    /** False means this profile neither preselects vision nor budgets for an
     *  mmproj. The two must move together: budgeting for a file the profile
     *  does not preselect shrinks what fits, for nothing. It never withholds the
     *  projector path, so image support stays a setting the user can turn on. */
    vision: z.boolean().optional(),
  })
  .strict();
export type FitProfileOverrides = z.infer<typeof fitProfileOverridesSchema>;

/** Data-driven fit policy, shipped in the catalog so tuning needs no release. */
export const fitConfigSchema = z
  .object({
    budgetFraction: budgetFractionSchema,
    safetyMarginBytes: z.number().int().nonnegative(),
    primaryScore: scoreSelectorSchema,
    /** Smallest bucket's quality floor = this model's primaryScore. */
    smallestQualityFloorRef: z.string(),
    /** Ordered, named rules applied when primary scores tie. Known rules:
     *  "lowerFootprint", "preferTag:<tag>". Unknown rules are ignored. */
    tieBreakers: z.array(z.string()).default([]),
    /** Fixed graph/activation slack added to every footprint. Defaulted rather
     *  than required so an already-signed catalog keeps today's behaviour. */
    computeOverheadBytes: z.number().int().nonnegative().default(DESKTOP_COMPUTE_OVERHEAD_BYTES),
    /** Largest context to offer per bucket, before the model's own max. */
    ctxCeiling: ctxCeilingSchema.default({ ...DESKTOP_CTX_CEILING }),
    /** Contexts tried, largest first. A context not on this list is not
     *  reachable at any budget: the fit engine walks the ladder and gives up. */
    ctxLadder: descendingLadder.default([...DESKTOP_CTX_LADDER]),
    /** Whether a model's vision add-on is preselected and budgeted for. Never
     *  gates the projector path, which is written on every profile. */
    vision: z.boolean().default(true),
    /** Named profiles overriding the above. `resolveFitProfile` is the only
     *  reader; nothing downstream sees an override. */
    profiles: z.object({ mobile: fitProfileOverridesSchema }).strict().optional(),
  })
  .strict();
export type FitConfig = z.infer<typeof fitConfigSchema>;

/** What a catalog AUTHOR writes: the defaulted fields are optional here and
 *  required on [`FitConfig`], which is the parsed shape every reader sees. */
export type FitConfigInput = z.input<typeof fitConfigSchema>;

/** Which device class the fit engine is deciding for. */
export type FitProfile = "desktop" | "mobile";

/** A [`FitConfig`] with a profile applied and every field concrete, so no
 *  consumer ever sees an optional. */
export type ResolvedFit = Omit<FitConfig, "profiles">;

/** Apply a profile's overrides to the base config.
 *
 *  `"desktop"` is the base unchanged, which is what makes adding a mobile
 *  profile inert on every existing install: there is no desktop branch to get
 *  wrong, because desktop IS the absence of one. */
export function resolveFitProfile(fit: FitConfig, profile: FitProfile = "desktop"): ResolvedFit {
  const { profiles: _profiles, ...base } = fit;
  if (profile === "desktop") return normaliseLadder(base);
  const over = fit.profiles?.mobile;
  if (!over) return normaliseLadder(base);
  return normaliseLadder({
    ...base,
    ...(over.budgetFraction ? { budgetFraction: over.budgetFraction } : {}),
    ...(over.safetyMarginBytes !== undefined ? { safetyMarginBytes: over.safetyMarginBytes } : {}),
    ...(over.smallestQualityFloorRef !== undefined
      ? { smallestQualityFloorRef: over.smallestQualityFloorRef }
      : {}),
    ...(over.computeOverheadBytes !== undefined
      ? { computeOverheadBytes: over.computeOverheadBytes }
      : {}),
    ...(over.ctxCeiling ? { ctxCeiling: over.ctxCeiling } : {}),
    ...(over.ctxLadder ? { ctxLadder: over.ctxLadder } : {}),
    ...(over.vision !== undefined ? { vision: over.vision } : {}),
  });
}

/** Sort the ladder descending, drop duplicates, fall back when it is empty.
 *
 *  The schema refuses an out-of-order ladder, so this only fires on a config
 *  that never met it. It exists because the Rust resolver does exactly the same
 *  thing, and two implementations that normalise differently IS F-A. */
function normaliseLadder(fit: ResolvedFit): ResolvedFit {
  const ladder = [...new Set(fit.ctxLadder)].sort((a, b) => b - a);
  return { ...fit, ctxLadder: ladder.length > 0 ? ladder : [...DESKTOP_CTX_LADDER] };
}

/** The fit profile a Core running on `os` should use.
 *
 *  `os` is the CORE'S OWN platform, not a field of `HardwareInfo`, which has no
 *  `os` and should not grow one for this: whether to apply a phone's memory
 *  policy is a fact about the process doing the fitting, and putting it on the
 *  wire would invite a client to send it.
 *
 *  Not derived from `Frontend::Embedded` either: a desktop process booted
 *  embedded still has desktop memory and would get a phone's budget, which is
 *  the line D-144 draws for tool workers. And not from a capability flag,
 *  because D-143 already ruled `llm.supportImages` the single source of truth
 *  for vision and a second one is the D-042 shape. */
export function fitProfileForOs(os: string): FitProfile {
  return os === "android" || os === "ios" ? "mobile" : "desktop";
}

/** The compiled catalog payload (everything that gets signed). */
export const catalogPayloadSchema = z
  .object({
    schemaVersion: z.literal(1),
    generatedAt: z.string(), // ISO 8601
    families: z.array(modelFamilySchema).min(1),
    fit: fitConfigSchema,
    stt: sttCatalogSchema,
    tts: ttsCatalogSchema,
  })
  .strict();
export type CatalogPayload = z.infer<typeof catalogPayloadSchema>;

/** The signed catalog as served + cached. `signature` is base64 Ed25519 over
 *  canonicalize(payload) (the whole object minus `signature`). */
export const modelCatalogSchema = catalogPayloadSchema
  .extend({
    signature: z.string(),
  })
  .strict();
export type ModelCatalog = z.infer<typeof modelCatalogSchema>;

/** Flatten every model across families (browser + fit both iterate models). */
export function catalogModels(c: CatalogPayload): Array<CatalogModel & { family: string }> {
  return c.families.flatMap((f) => f.models.map((m) => ({ ...m, family: f.family })));
}

/** Resolve the primary "smartness" score for a model, or undefined if the model
 *  carries no score for the configured selector. */
export function primaryScore(model: CatalogModel, sel: ScoreSelector): number | undefined {
  return model.scores.find((s) => s.source === sel.source && s.metric === sel.metric)?.value;
}
