// @loca/model-catalog: the hand-authored source for Loca's local-model
// catalog. `scripts/release/catalog.ts` consumes buildCatalogPayload(), signs
// it, and publishes catalog.json. See README.md for the authoring workflow.

import { type CatalogPayload, catalogPayloadSchema, type ModelFamily } from "@loca/shared";
import { FAMILIES } from "./families/index.ts";
import { FIT_CONFIG } from "./fit.ts";
import { FILE_HASHES } from "./hashes.generated.ts";
import { STT_CATALOG } from "./stt.ts";
import { TTS_CATALOG } from "./tts.ts";

export { FAMILIES } from "./families/index.ts";
export { FIT_CONFIG } from "./fit.ts";
export { STT_CATALOG } from "./stt.ts";
export { TTS_CATALOG } from "./tts.ts";

/** Assemble + validate the catalog payload (everything that gets signed).
 *  `generatedAt` is passed in (scripts stamp it) since the scripts environment
 *  forbids ambient clock access in some contexts. Throws on schema violations. */
export function buildCatalogPayload(
  generatedAt: string,
  families: ModelFamily[] = FAMILIES,
): CatalogPayload {
  const payload = {
    schemaVersion: 1 as const,
    generatedAt,
    families,
    fit: FIT_CONFIG,
    stt: STT_CATALOG,
    tts: TTS_CATALOG,
  };
  const parsed = catalogPayloadSchema.parse(payload);
  stampHashes(parsed);
  checkSpeechReferences("stt", parsed.stt.models, parsed.stt.presets);
  checkSpeechReferences("tts", parsed.tts.models, parsed.tts.presets);
  return parsed;
}

/** Stamp each downloadable file's pinned sha256 (from the generated
 *  HuggingFace-gathered map) onto the parsed payload, so the signed catalog
 *  commits to the exact bytes and the download path verifies against it. A spec
 *  absent from the map is left unstamped (the download then falls back to HF's
 *  published hash); `deno run -A scripts/catalog/gather-hashes.ts` refills the
 *  map when a model is added. Mutates the freshly-parsed copy (not the imported
 *  source constants). */
function stampHashes(payload: CatalogPayload): void {
  for (const fam of payload.families) {
    for (const model of fam.models) {
      for (const v of model.variants) {
        if (v.mmprojSpec && FILE_HASHES[v.mmprojSpec]) {
          v.mmprojSha256 = FILE_HASHES[v.mmprojSpec];
        }
        for (const q of v.quants) {
          const h = FILE_HASHES[q.modelSpec];
          if (h) q.sha256 = h;
        }
      }
    }
  }
  for (const cat of [payload.stt, payload.tts]) {
    for (const model of cat.models) {
      for (const quant of model.quants) {
        for (const f of quant.files) {
          const h = FILE_HASHES[f.modelSpec];
          if (h) f.sha256 = h;
        }
      }
    }
  }
}

/** Referential checks zod can't express without ZodEffects (which would break
 *  the `.extend()` building modelCatalogSchema): unique model + preset ids, and
 *  every preset pointing at an existing model+quant. Shared by stt and tts since
 *  their model/preset shapes are identical here. */
function checkSpeechReferences(
  kind: "stt" | "tts",
  models: { id: string; quants: { quant: string }[] }[],
  presets: { id: string; modelId: string; quant: string }[],
): void {
  const modelIds = new Set<string>();
  for (const model of models) {
    if (modelIds.has(model.id)) {
      throw new Error(`duplicate ${kind} model id: ${model.id}`);
    }
    modelIds.add(model.id);
  }
  const presetIds = new Set<string>();
  for (const preset of presets) {
    if (presetIds.has(preset.id)) {
      throw new Error(`duplicate ${kind} preset id: ${preset.id}`);
    }
    presetIds.add(preset.id);
    const model = models.find((m) => m.id === preset.modelId);
    if (!model) {
      throw new Error(`${kind} preset "${preset.id}" references unknown model: ${preset.modelId}`);
    }
    if (!model.quants.some((q) => q.quant === preset.quant)) {
      throw new Error(
        `${kind} preset "${preset.id}" references unknown quant ${preset.quant} of ${preset.modelId}`,
      );
    }
  }
}
