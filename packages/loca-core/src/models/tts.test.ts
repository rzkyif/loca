// Text-to-Speech catalog resolution, exercised against the real authored
// catalog (network-free, like stt.test.ts). Also guards the cross-package
// agreements: settings-schema card ids vs catalog presets, and the
// tts.modelPath schema default vs catalog specs.

import { assert, assertEquals, assertThrows } from "@std/assert";
import { SETTINGS_SCHEMA, type TtsPresetField } from "@loca/shared";
import { primaryFileSpec, TTS_PRIMARY_ROLE } from "@loca/shared";
import { buildCatalogPayload } from "../../../loca-model-catalog/src/index.ts";
import { buildTtsPresetViews, resolveTtsSelection, selectedTtsVoices } from "./tts.ts";
import { AppError } from "@loca/core-engine";

const catalog = buildCatalogPayload("2026-06-15T00:00:00Z");

function presetField(): TtsPresetField {
  for (const group of SETTINGS_SCHEMA) {
    for (const section of group.sections) {
      for (const field of section.fields) {
        if (field.id === "tts.preset" && field.type === "tts_preset") {
          return field;
        }
      }
    }
  }
  throw new Error("tts.preset tts_preset field not found in settings schema");
}

Deno.test("tts: settings-schema cards and catalog presets agree", () => {
  const cardIds = presetField()
    .presetConfig.options.map((o) => o.id)
    .sort();
  const presetIds = catalog.tts.presets.map((p) => p.id).sort();
  assertEquals(cardIds, presetIds);
});

Deno.test("tts: the tts.modelPath schema default is a catalog spec", () => {
  const field = SETTINGS_SCHEMA.flatMap((g) => g.sections)
    .flatMap((s) => s.fields)
    .find((f) => f.id === "tts.modelPath");
  assert(field, "tts.modelPath field should exist");
  const specs = catalog.tts.models.flatMap((m) =>
    m.quants.map((q) => primaryFileSpec(q, TTS_PRIMARY_ROLE[m.family])),
  );
  assert(
    specs.includes(field!.defaultValue as string),
    `tts.modelPath default not in catalog: ${field!.defaultValue}`,
  );
});

Deno.test("tts: the default card matches the tts.modelPath schema default", () => {
  const field = presetField();
  const applied = resolveTtsSelection(catalog, {
    presetId: field.defaultValue,
  });
  const modelPathDefault = SETTINGS_SCHEMA.flatMap((g) => g.sections)
    .flatMap((s) => s.fields)
    .find((f) => f.id === "tts.modelPath")!.defaultValue;
  assertEquals(applied.settings.modelPath, modelPathDefault);
});

Deno.test("tts: preset views resolve every curated card with its quant + voices", () => {
  const views = buildTtsPresetViews(catalog);
  assertEquals(views.length, catalog.tts.presets.length);
  for (const view of views) {
    assert(view.modelSpec.startsWith("@"), `spec should be an HF spec: ${view.modelSpec}`);
    assert(view.fileSizeBytes > 0);
    assert(view.voices.length > 0, `card ${view.id} should expose voices`);
    assert(
      view.voices.some((v) => v.id === view.defaultVoice),
      `default voice ${view.defaultVoice} should be in the voice list`,
    );
  }
});

Deno.test("tts: presetId selection keeps the preset id and sets the model bundle", () => {
  const applied = resolveTtsSelection(catalog, { presetId: "kitten" });
  assertEquals(applied.preset, "kitten");
  assertEquals(applied.settings.modelType, "kitten");
  assertEquals(
    applied.settings.modelPath,
    "@csukuangfj/kitten-nano-en-v0_1-fp16/main/model.fp16.onnx",
  );
  // modelFiles is the JSON role->spec bundle the sidecar reads.
  const files = JSON.parse(applied.settings.modelFiles) as Record<string, string>;
  assertEquals(typeof files.model, "string");
  assertEquals(typeof files.voices, "string");
  assertEquals(typeof files.tokens, "string");
});

Deno.test("tts: a matcha selection carries the acoustic_model + vocoder roles", () => {
  const applied = resolveTtsSelection(catalog, { modelId: "matcha-ljspeech" });
  assertEquals(applied.preset, "custom");
  const files = JSON.parse(applied.settings.modelFiles) as Record<string, string>;
  assert(files.acoustic_model?.startsWith("@"));
  assert(files.vocoder?.startsWith("@"));
});

Deno.test("tts: unknown selectors throw not_found; empty body throws validation_error", () => {
  for (const body of [
    { presetId: "nope" },
    { modelId: "nope" },
    { modelSpec: "@nope/nope/main/nope.onnx" },
  ]) {
    const err = assertThrows(() => resolveTtsSelection(catalog, body), AppError);
    assertEquals(err.code, "not_found");
  }
  const err = assertThrows(() => resolveTtsSelection(catalog, {}), AppError);
  assertEquals(err.code, "validation_error");
});

Deno.test("tts: selectedTtsVoices resolves the model's voices, falling back by family", () => {
  const kokoroPath = "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/model.int8.onnx";
  assert(selectedTtsVoices(catalog, "kokoro", kokoroPath).length > 1);
  // Unknown path but known family still resolves voices by family.
  assert(selectedTtsVoices(catalog, "kitten", "@made/up/main/x.onnx").length > 0);
  // Unknown family yields no voices.
  assertEquals(selectedTtsVoices(catalog, "nope", "").length, 0);
});
