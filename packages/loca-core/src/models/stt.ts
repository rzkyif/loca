// Speech-to-Text catalog resolution.
//
// The whisper picker has no fit engine: int8 bundles span ~100 MB to ~1 GB, so
// everything "fits" and selection is a direct lookup. This module resolves the
// curated cards and turns a selection into the stt.* settings to apply,
// mirroring the role fit.ts plays for the LLM picker.

import type {
  AppliedSttSettings,
  CatalogPayload,
  HardwareInfo,
  SpeechQuant,
  SttCatalogModel,
  SttPresetView,
} from "@loca/shared";
import { modelFilesObject, primaryFileSpec, STT_PRIMARY_ROLE } from "@loca/shared";
import { AppError } from "@loca/core-engine";

const MAX_THREADS = 8;

/** Threads for the speech sidecar's Whisper engine: one per physical core,
 *  capped (same policy as the LLM fit engine). */
export function sttThreads(hw: HardwareInfo): number {
  return Math.max(1, Math.min(hw.cpuCoresPhysical || 4, MAX_THREADS));
}

/** Resolve the curated cards against the catalog (badges for the client).
 *  Presets are validated against the models at catalog build time, so a
 *  dangling reference here is a bug, not bad data; it is skipped defensively. */
export function buildSttPresetViews(catalog: CatalogPayload): SttPresetView[] {
  const views: SttPresetView[] = [];
  for (const preset of catalog.stt.presets) {
    const model = catalog.stt.models.find((m) => m.id === preset.modelId);
    const quant = model?.quants.find((q) => q.quant === preset.quant);
    if (!model || !quant) continue;
    views.push({
      id: preset.id,
      modelId: model.id,
      name: model.name,
      family: model.family,
      english: model.english,
      quant: quant.quant,
      modelSpec: primaryFileSpec(quant, STT_PRIMARY_ROLE[model.family]),
      fileSizeBytes: quant.fileSizeBytes,
    });
  }
  return views;
}

/** Default quant when a model is picked without one: int8 (near-lossless and the
 *  only quant the catalog ships), else the first quant. */
function defaultQuant(model: SttCatalogModel): SpeechQuant {
  return model.quants.find((q) => q.quant === "int8") ?? model.quants[0];
}

function appliedStt(
  model: SttCatalogModel,
  quant: SpeechQuant,
  hw: HardwareInfo,
): AppliedSttSettings {
  return {
    modelType: model.family,
    modelPath: primaryFileSpec(quant, STT_PRIMARY_ROLE[model.family]),
    modelFiles: JSON.stringify(modelFilesObject(quant)),
    threads: sttThreads(hw),
  };
}

/** Resolve a {presetId|modelId|modelSpec} selection into the stt.* settings to
 *  apply. presetId keeps its id as the preset; a manual model/quant pick is
 *  "custom". modelSpec matches a model's primary (identity) file. */
export function resolveSttSelection(
  catalog: CatalogPayload,
  hw: HardwareInfo,
  body: { presetId?: string; modelId?: string; modelSpec?: string },
): { preset: string; settings: AppliedSttSettings } {
  const stt = catalog.stt;
  if (body.presetId) {
    const preset = stt.presets.find((p) => p.id === body.presetId);
    const model = preset && stt.models.find((m) => m.id === preset.modelId);
    const quant = model?.quants.find((q) => q.quant === preset?.quant);
    if (!preset || !model || !quant) {
      throw new AppError("not_found", `preset not in catalog: ${body.presetId}`);
    }
    return { preset: preset.id, settings: appliedStt(model, quant, hw) };
  }
  if (body.modelSpec) {
    for (const model of stt.models) {
      const quant = model.quants.find(
        (q) => primaryFileSpec(q, STT_PRIMARY_ROLE[model.family]) === body.modelSpec,
      );
      if (quant) {
        return { preset: "custom", settings: appliedStt(model, quant, hw) };
      }
    }
    throw new AppError("not_found", `model not in catalog: ${body.modelSpec}`);
  }
  if (body.modelId) {
    const model = stt.models.find((m) => m.id === body.modelId);
    if (!model) {
      throw new AppError("not_found", `model not in catalog: ${body.modelId}`);
    }
    return {
      preset: "custom",
      settings: appliedStt(model, defaultQuant(model), hw),
    };
  }
  throw new AppError("validation_error", "presetId, modelId, or modelSpec required");
}
