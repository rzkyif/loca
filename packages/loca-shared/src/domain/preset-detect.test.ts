// detectLlmPreset: matches a settings snapshot back to a smart preset (so the
// settings UI can restore one instead of always flipping to Custom), else
// "custom". Pure function, no I/O.

import { assertEquals } from "@std/assert";
import type { AppliedModelSettings } from "./recommend.ts";
import { detectLlmPreset } from "./preset-detect.ts";

const APPLY: AppliedModelSettings = {
  modelPath: "@unsloth/Qwen3.5-0.8B-GGUF/main/Qwen3.5-0.8B-Q4_K_M.gguf",
  mmprojPath: "@unsloth/Qwen3.5-0.8B-GGUF/main/mmproj-F16.gguf",
  contextSize: 4096,
  threads: 4,
  gpuLayers: 0,
  flashAttn: false,
  supportImages: true,
  idleUnloadSeconds: 0,
  temperature: 0.7,
  topP: 0.8,
  topK: 20,
  minP: 0,
  repeatPenalty: 1.05,
  reasoningBudget: 512,
};

function settingsFor(apply: AppliedModelSettings): Record<string, unknown> {
  return {
    "llm.modelPath": apply.modelPath,
    "llm.mmprojPath": apply.mmprojPath,
    "llm.contextSize": apply.contextSize,
    "llm.threads": apply.threads,
    "llm.gpuLayers": apply.gpuLayers,
    "llm.flashAttn": apply.flashAttn,
    "llm.supportImages": apply.supportImages,
    "llm.idleUnloadSeconds": apply.idleUnloadSeconds,
    "llm.topP": apply.topP,
    "llm.topK": apply.topK,
    "llm.minP": apply.minP,
    "llm.repeatPenalty": apply.repeatPenalty,
  };
}

Deno.test("detectLlmPreset: returns the bucket when every managed field matches", () => {
  assertEquals(detectLlmPreset(settingsFor(APPLY), { smallest: APPLY }), "smallest");
});

Deno.test("detectLlmPreset: a single mismatched managed field falls to custom", () => {
  const s = { ...settingsFor(APPLY), "llm.threads": 8 };
  assertEquals(detectLlmPreset(s, { smallest: APPLY }), "custom");
});

Deno.test("detectLlmPreset: behavior prefs (temperature/budget) don't affect the match", () => {
  const s = {
    ...settingsFor(APPLY),
    "llm.temperature": 1.2,
    "llm.reasoningBudget": 9999,
  };
  assertEquals(detectLlmPreset(s, { smallest: APPLY }), "smallest");
});

Deno.test("detectLlmPreset: toggling Image Attachments doesn't affect the match", () => {
  // supportImages is a user choice, not part of the model identity: flipping it
  // must keep the preset, not drop to custom.
  const s = { ...settingsFor(APPLY), "llm.supportImages": !APPLY.supportImages };
  assertEquals(detectLlmPreset(s, { smallest: APPLY }), "smallest");
});

Deno.test("detectLlmPreset: coerces numeric strings (e.g. gpuLayers) before comparing", () => {
  const s = {
    ...settingsFor(APPLY),
    "llm.gpuLayers": "0",
    "llm.contextSize": "4096",
  };
  assertEquals(detectLlmPreset(s, { smallest: APPLY }), "smallest");
});

Deno.test("detectLlmPreset: empty applied vision path matches an unset key", () => {
  const apply = { ...APPLY, mmprojPath: undefined, supportImages: false };
  const s = { ...settingsFor(apply), "llm.mmprojPath": "" };
  assertEquals(detectLlmPreset(s, { half: apply }), "half");
});

Deno.test("detectLlmPreset: no recommendations loaded returns custom", () => {
  assertEquals(detectLlmPreset(settingsFor(APPLY), {}), "custom");
});

Deno.test("detectLlmPreset: a null bucket is skipped", () => {
  assertEquals(detectLlmPreset(settingsFor(APPLY), { smallest: null, full: APPLY }), "full");
});
