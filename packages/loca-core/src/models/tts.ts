// Text-to-Speech catalog resolution. Mirrors models/stt.ts: TTS has no fit
// engine (bundles are small and all "fit"), so selection is a direct lookup that
// turns a {presetId|modelId|modelSpec} choice into the tts.* settings to apply.

import type {
  AppliedTtsSettings,
  CatalogPayload,
  SpeechQuant,
  TtsCatalogModel,
  TtsPresetView,
} from "@loca/shared";
import { modelFilesObject, primaryFileSpec, TTS_PRIMARY_ROLE } from "@loca/shared";
import { AppError } from "@loca/core-engine";

/** Resolve the curated cards against the catalog (badges + voices for the
 *  client). Presets are validated at catalog build time, so a dangling reference
 *  here is a bug, not bad data; it is skipped defensively. */
export function buildTtsPresetViews(catalog: CatalogPayload): TtsPresetView[] {
  const views: TtsPresetView[] = [];
  for (const preset of catalog.tts.presets) {
    const model = catalog.tts.models.find((m) => m.id === preset.modelId);
    const quant = model?.quants.find((q) => q.quant === preset.quant);
    if (!model || !quant) continue;
    views.push({
      id: preset.id,
      modelId: model.id,
      name: model.name,
      family: model.family,
      quant: quant.quant,
      modelSpec: primaryFileSpec(quant, TTS_PRIMARY_ROLE[model.family]),
      fileSizeBytes: quant.fileSizeBytes,
      voices: model.voices,
      defaultVoice: model.defaultVoice,
    });
  }
  return views;
}

/** Default quant when a model is picked without one: the first listed (best). */
function defaultQuant(model: TtsCatalogModel): SpeechQuant {
  return model.quants[0];
}

function appliedTts(model: TtsCatalogModel, quant: SpeechQuant): AppliedTtsSettings {
  return {
    modelType: model.family,
    modelPath: primaryFileSpec(quant, TTS_PRIMARY_ROLE[model.family]),
    modelFiles: JSON.stringify(modelFilesObject(quant), null, 2),
    voice: model.defaultVoice,
  };
}

/** Resolve a {presetId|modelId|modelSpec} selection into the tts.* settings to
 *  apply. presetId keeps its id as the preset; a manual model/quant pick is
 *  "custom". modelSpec matches a model's primary (identity) file. */
export function resolveTtsSelection(
  catalog: CatalogPayload,
  body: { presetId?: string; modelId?: string; modelSpec?: string },
): { preset: string; settings: AppliedTtsSettings } {
  const tts = catalog.tts;
  if (body.presetId) {
    const preset = tts.presets.find((p) => p.id === body.presetId);
    const model = preset && tts.models.find((m) => m.id === preset.modelId);
    const quant = model?.quants.find((q) => q.quant === preset?.quant);
    if (!preset || !model || !quant) {
      throw new AppError("not_found", `preset not in catalog: ${body.presetId}`);
    }
    return { preset: preset.id, settings: appliedTts(model, quant) };
  }
  if (body.modelSpec) {
    for (const model of tts.models) {
      const quant = model.quants.find(
        (q) => primaryFileSpec(q, TTS_PRIMARY_ROLE[model.family]) === body.modelSpec,
      );
      if (quant) {
        return { preset: "custom", settings: appliedTts(model, quant) };
      }
    }
    throw new AppError("not_found", `model not in catalog: ${body.modelSpec}`);
  }
  if (body.modelId) {
    const model = tts.models.find((m) => m.id === body.modelId);
    if (!model) {
      throw new AppError("not_found", `model not in catalog: ${body.modelId}`);
    }
    return {
      preset: "custom",
      settings: appliedTts(model, defaultQuant(model)),
    };
  }
  throw new AppError("validation_error", "presetId, modelId, or modelSpec required");
}

/** The voices of the TTS model currently selected by `tts.modelType` +
 *  `tts.modelPath`, for GET /tts/voices. Falls back to the default-preset model
 *  when settings don't match a catalog model (e.g. a hand-typed custom path). */
export function selectedTtsVoices(
  catalog: CatalogPayload,
  modelType: string,
  modelPath: string,
): { id: string; label: string; lang?: string }[] {
  const byPath = catalog.tts.models.find(
    (m) =>
      m.family === modelType &&
      m.quants.some((q) => primaryFileSpec(q, TTS_PRIMARY_ROLE[m.family]) === modelPath),
  );
  if (byPath) return byPath.voices;
  const byType = catalog.tts.models.find((m) => m.family === modelType);
  return byType?.voices ?? [];
}
