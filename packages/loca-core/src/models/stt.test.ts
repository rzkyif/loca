// Speech-to-Text catalog resolution, exercised against the real authored
// catalog (network-free, like fit.test.ts). Also guards the cross-package
// agreements: settings-schema card ids vs catalog presets, and the
// stt.modelPath schema default vs catalog specs.

import { assert, assertEquals, assertThrows } from "@std/assert";
import { type HardwareInfo, SETTINGS_SCHEMA, type SttPresetField } from "@loca/shared";
import { primaryFileSpec, STT_PRIMARY_ROLE } from "@loca/shared";
import { buildCatalogPayload } from "../../../loca-model-catalog/src/index.ts";
import { buildSttPresetViews, resolveSttSelection, sttThreads } from "./stt.ts";
import { AppError } from "@loca/core-engine";

const catalog = buildCatalogPayload("2026-06-11T00:00:00Z");

function hw(cores: number): HardwareInfo {
  return {
    totalRamBytes: 16e9,
    availableRamBytes: 8e9,
    cpuCoresPhysical: cores,
    gpu: { backend: "cpu", name: "cpu", vramBytes: 0 },
    unifiedMemory: false,
  };
}

function presetField(): SttPresetField {
  for (const group of SETTINGS_SCHEMA) {
    for (const section of group.sections) {
      for (const field of section.fields) {
        if (field.id === "stt.preset" && field.type === "stt_preset") {
          return field;
        }
      }
    }
  }
  throw new Error("stt.preset stt_preset field not found in settings schema");
}

Deno.test("stt: settings-schema cards and catalog presets agree", () => {
  const cardIds = presetField()
    .presetConfig.options.map((o) => o.id)
    .sort();
  const presetIds = catalog.stt.presets.map((p) => p.id).sort();
  assertEquals(cardIds, presetIds);
});

Deno.test("stt: the stt.modelPath schema default is a catalog spec", () => {
  const field = SETTINGS_SCHEMA.flatMap((g) => g.sections)
    .flatMap((s) => s.fields)
    .find((f) => f.id === "stt.modelPath");
  assert(field, "stt.modelPath field should exist");
  const specs = catalog.stt.models.flatMap((m) =>
    m.quants.map((q) => primaryFileSpec(q, STT_PRIMARY_ROLE[m.family])),
  );
  assert(
    specs.includes(field!.defaultValue as string),
    `stt.modelPath default not in catalog: ${field!.defaultValue}`,
  );
});

Deno.test("stt: the default card matches the stt.modelPath schema default", () => {
  const field = presetField();
  const applied = resolveSttSelection(catalog, hw(4), {
    presetId: field.defaultValue,
  });
  const modelPathDefault = SETTINGS_SCHEMA.flatMap((g) => g.sections)
    .flatMap((s) => s.fields)
    .find((f) => f.id === "stt.modelPath")!.defaultValue;
  assertEquals(applied.settings.modelPath, modelPathDefault);
});

Deno.test("stt: preset views resolve every curated card with its quant", () => {
  const views = buildSttPresetViews(catalog);
  assertEquals(views.length, catalog.stt.presets.length);
  for (const view of views) {
    assert(view.modelSpec.startsWith("@"), `spec should be an HF spec: ${view.modelSpec}`);
    assert(view.fileSizeBytes > 0);
  }
});

Deno.test("stt: presetId selection keeps the preset id", () => {
  const applied = resolveSttSelection(catalog, hw(4), { presetId: "accurate" });
  assertEquals(applied.preset, "accurate");
  assertEquals(
    applied.settings.modelPath,
    "@csukuangfj/sherpa-onnx-whisper-turbo/main/turbo-encoder.int8.onnx",
  );
});

Deno.test("stt: modelId selection applies the default quant (int8)", () => {
  const applied = resolveSttSelection(catalog, hw(4), {
    modelId: "whisper-medium.en",
  });
  assertEquals(applied.preset, "custom");
  assertEquals(
    applied.settings.modelPath,
    "@csukuangfj/sherpa-onnx-whisper-medium.en/main/medium.en-encoder.int8.onnx",
  );
});

Deno.test("stt: modelId selection resolves a model's int8 bundle (tiny)", () => {
  const applied = resolveSttSelection(catalog, hw(4), {
    modelId: "whisper-tiny",
  });
  assertEquals(applied.preset, "custom");
  assertEquals(
    applied.settings.modelPath,
    "@csukuangfj/sherpa-onnx-whisper-tiny/main/tiny-encoder.int8.onnx",
  );
});

Deno.test("stt: modelSpec selection picks the exact quant as custom", () => {
  const spec = "@csukuangfj/sherpa-onnx-whisper-base/main/base-encoder.int8.onnx";
  const applied = resolveSttSelection(catalog, hw(4), { modelSpec: spec });
  assertEquals(applied.preset, "custom");
  assertEquals(applied.settings.modelPath, spec);
});

Deno.test("stt: unknown selectors throw not_found; empty body throws validation_error", () => {
  for (const body of [
    { presetId: "nope" },
    { modelId: "nope" },
    { modelSpec: "@nope/nope/main/nope.bin" },
  ]) {
    const err = assertThrows(() => resolveSttSelection(catalog, hw(4), body), AppError);
    assertEquals(err.code, "not_found");
  }
  const err = assertThrows(() => resolveSttSelection(catalog, hw(4), {}), AppError);
  assertEquals(err.code, "validation_error");
});

Deno.test("stt: threads track physical cores, clamped to 1..8", () => {
  assertEquals(sttThreads(hw(1)), 1);
  assertEquals(sttThreads(hw(6)), 6);
  assertEquals(sttThreads(hw(16)), 8);
  assertEquals(sttThreads(hw(0)), 4); // unknown core count falls back to 4
});
