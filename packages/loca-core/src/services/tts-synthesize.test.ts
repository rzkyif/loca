import { assertEquals, assertRejects } from "@std/assert";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { paths } from "../paths.ts";
import { synthesizeSpeech } from "@loca/core-engine/services/tts-synthesize";
import { __resetForTesting as resetCoreSettings } from "@loca/core-engine/services/core-settings";
import { AppError } from "@loca/core-engine";

async function writeSettings(s: Record<string, unknown>): Promise<void> {
  await Deno.writeTextFile(paths().settingsFile, JSON.stringify(s));
  resetCoreSettings();
}

Deno.test("synthesizeSpeech: external provider posts to the OpenAI speech endpoint with the configured voice", async () => {
  const env = await setupTestEnv();
  const realFetch = globalThis.fetch;
  try {
    await writeSettings({
      "tts.provider": "external",
      "tts.external.baseUrl": "https://api.example.com/v1",
      "tts.external.model": "tts-1",
      "tts.external.apiKey": "sk-real",
      "tts.external.voice": "alloy",
    });

    const wav = new Uint8Array([0x52, 0x49, 0x46, 0x46, 1, 2, 3, 4]); // "RIFF"...
    let calledUrl = "";
    let sentVoice = "";
    globalThis.fetch = ((input: string | URL | Request, init?: RequestInit) => {
      calledUrl = typeof input === "string" ? input : input.toString();
      sentVoice = JSON.parse(String(init?.body ?? "{}")).voice ?? "";
      return Promise.resolve(new Response(wav, { status: 200 }));
    }) as typeof fetch;

    // The caller passes a local Kokoro voice id; the external path must ignore
    // it and use tts.external.voice instead.
    const out = await synthesizeSpeech("hello world", "af_bella", undefined, "test-client");
    assertEquals(out, wav);
    assertEquals(calledUrl.includes("/audio/speech"), true);
    assertEquals(sentVoice, "alloy");
  } finally {
    globalThis.fetch = realFetch;
    await env.teardown();
  }
});

Deno.test("synthesizeSpeech: external provider without baseUrl/model is a validation error", async () => {
  const env = await setupTestEnv();
  try {
    await writeSettings({ "tts.provider": "external" });
    const err = await assertRejects(() => synthesizeSpeech("hi"), AppError);
    assertEquals(err.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("synthesizeSpeech: external provider without an API key is a validation error", async () => {
  const env = await setupTestEnv();
  try {
    await writeSettings({
      "tts.provider": "external",
      "tts.external.baseUrl": "https://api.example.com/v1",
      "tts.external.model": "tts-1",
    });
    const err = await assertRejects(() => synthesizeSpeech("hi"), AppError);
    assertEquals(err.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("synthesizeSpeech: external 401 maps to provider_unauthorized", async () => {
  const env = await setupTestEnv();
  const realFetch = globalThis.fetch;
  try {
    await writeSettings({
      "tts.provider": "external",
      "tts.external.baseUrl": "https://api.example.com/v1",
      "tts.external.model": "tts-1",
      "tts.external.apiKey": "sk-bad",
    });
    globalThis.fetch = (() =>
      Promise.resolve(new Response("nope", { status: 401 }))) as typeof fetch;
    const err = await assertRejects(() => synthesizeSpeech("hi"), AppError);
    assertEquals(err.code, "provider_unauthorized");
  } finally {
    globalThis.fetch = realFetch;
    await env.teardown();
  }
});
