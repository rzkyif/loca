// model-ensure surface. Tested for settings -> sources mapping (no I/O
// beyond settings file read) and ensureKindModels' "already have" branch
// (pre-create the files so probe() returns without touching the network).

import { assertEquals } from "@std/assert";
import { join } from "@std/path";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { paths } from "../paths.ts";
import { patchCoreSettings } from "@loca/core-engine/services/core-settings";
import { ensureKindModels, sourcesForKind } from "./model-ensure.ts";

Deno.test("sourcesForKind('llm'): empty when llm.provider is external", async () => {
  const env = await setupTestEnv();
  try {
    await patchCoreSettings({
      "llm.provider": "external",
      "llm.modelPath": "@u/r/main/x.gguf",
    });
    assertEquals(await sourcesForKind("llm"), []);
  } finally {
    await env.teardown();
  }
});

Deno.test("sourcesForKind('llm'): returns model + mmproj when supportImages is true", async () => {
  const env = await setupTestEnv();
  try {
    await patchCoreSettings({
      "llm.provider": "local",
      "llm.modelPath": "@u/r/main/m.gguf",
      "llm.supportImages": true,
      "llm.mmprojPath": "@u/r/main/mm.gguf",
    });
    const sources = await sourcesForKind("llm");
    assertEquals(sources, ["@u/r/main/m.gguf", "@u/r/main/mm.gguf"]);
  } finally {
    await env.teardown();
  }
});

Deno.test("sourcesForKind('llm'): omits mmproj when supportImages is false", async () => {
  const env = await setupTestEnv();
  try {
    await patchCoreSettings({
      "llm.provider": "local",
      "llm.modelPath": "@u/r/main/m.gguf",
      "llm.supportImages": false,
      "llm.mmprojPath": "@u/r/main/mm.gguf",
    });
    const sources = await sourcesForKind("llm");
    assertEquals(sources, ["@u/r/main/m.gguf"]);
  } finally {
    await env.teardown();
  }
});

Deno.test("sourcesForKind('stt'): empty when stt.enabled is false", async () => {
  const env = await setupTestEnv();
  try {
    await patchCoreSettings({
      "stt.enabled": false,
      "stt.modelPath": "@u/r/main/whisper.bin",
    });
    assertEquals(await sourcesForKind("stt"), []);
  } finally {
    await env.teardown();
  }
});

Deno.test("sourcesForKind('stt'): empty when stt.provider is external", async () => {
  const env = await setupTestEnv();
  try {
    await patchCoreSettings({
      "stt.enabled": true,
      "stt.provider": "external",
      "stt.modelPath": "@u/r/main/whisper.bin",
    });
    assertEquals(await sourcesForKind("stt"), []);
  } finally {
    await env.teardown();
  }
});

Deno.test("sourcesForKind('embed'): always returns the fixed base files", async () => {
  const env = await setupTestEnv();
  try {
    const embed = await sourcesForKind("embed");
    // The exact set is governed by shared constants; here we only assert the
    // wiring (non-empty, every item is an HF spec).
    assertEquals(embed.length > 0, true);
    for (const s of embed) assertEquals(s.startsWith("@"), true);
  } finally {
    await env.teardown();
  }
});

Deno.test("sourcesForKind('tts'): empty when disabled, base files when enabled", async () => {
  const env = await setupTestEnv();
  try {
    // tts.enabled defaults to false.
    assertEquals(await sourcesForKind("tts"), []);
    await patchCoreSettings({ "tts.enabled": true });
    const tts = await sourcesForKind("tts");
    assertEquals(tts.length > 0, true);
    for (const s of tts) assertEquals(s.startsWith("@"), true);
  } finally {
    await env.teardown();
  }
});

Deno.test("ensureKindModels: skips network when every source is already on disk", async () => {
  const env = await setupTestEnv();
  try {
    await patchCoreSettings({
      "llm.provider": "local",
      "llm.modelPath": "@u/r/main/m.gguf",
      "llm.supportImages": false,
    });
    // Pre-create the file at the resolved abs path so probeSource short-circuits.
    const rel = "u/r/m.gguf";
    const abs = join(paths().modelsDir, rel);
    await Deno.mkdir(join(paths().modelsDir, "u", "r"), { recursive: true });
    await Deno.writeTextFile(abs, "stub");

    const out = await ensureKindModels("llm");
    assertEquals(out.enqueued, []);
    assertEquals(out.alreadyHave, ["@u/r/main/m.gguf"]);
  } finally {
    await env.teardown();
  }
});

Deno.test("ensureKindModels: returns empty pair when sourcesForKind is empty", async () => {
  const env = await setupTestEnv();
  try {
    await patchCoreSettings({ "stt.enabled": false });
    const out = await ensureKindModels("stt");
    assertEquals(out, { enqueued: [], alreadyHave: [] });
  } finally {
    await env.teardown();
  }
});
