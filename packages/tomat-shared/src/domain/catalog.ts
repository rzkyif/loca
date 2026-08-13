// Model catalog: the hand-authored, signed list of local models Tomat can run,
// served at get.au.tomat.ing/manifests[/<channel>]/catalog.json.
//
// Authoring lives in the `@tomat/model-catalog` package as one TS file per model
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

/** One authoring file in `@tomat/model-catalog`. */
export const modelFamilySchema = z
  .object({
    family: z.string(), // "Qwen 3.5"
    publisher: z.string(), // "Qwen"
    models: z.array(catalogModelSchema).min(1),
  })
  .strict();
export type ModelFamily = z.infer<typeof modelFamilySchema>;

/** Which sherpa-onnx offline recognizer config a Speech-to-Text model loads
 *  under. The sidecar (and its Rust `SttConfig` enum) dispatch on this. */
export const sttFamilySchema = z.enum([
  "whisper",
  "sense-voice",
  "moonshine",
  "paraformer",
  "transducer",
  "nemo-ctc",
  "dolphin",
  "telespeech-ctc",
]);
export type SttFamily = z.infer<typeof sttFamilySchema>;

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

/** Which sherpa-onnx offline TTS config a Text-to-Speech model loads under. */
export const ttsFamilySchema = z.enum(["kokoro", "kitten", "vits", "matcha"]);
export type TtsFamily = z.infer<typeof ttsFamilySchema>;

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

/** Data-driven fit policy, shipped in the catalog so tuning needs no release. */
export const fitConfigSchema = z
  .object({
    budgetFraction: z
      .object({
        half: z.number().positive().max(1),
        full: z.number().positive().max(1),
      })
      .strict(),
    safetyMarginBytes: z.number().int().nonnegative(),
    primaryScore: scoreSelectorSchema,
    /** Smallest bucket's quality floor = this model's primaryScore. */
    smallestQualityFloorRef: z.string(),
    /** Ordered, named rules applied when primary scores tie. Known rules:
     *  "lowerFootprint", "preferTag:<tag>". Unknown rules are ignored. */
    tieBreakers: z.array(z.string()).default([]),
  })
  .strict();
export type FitConfig = z.infer<typeof fitConfigSchema>;

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
