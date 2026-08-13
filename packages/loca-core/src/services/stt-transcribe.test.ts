import { assertEquals, assertRejects } from "@std/assert";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { transcribeAudio } from "@loca/core-engine/services/stt-transcribe";
import { AppError } from "@loca/core-engine";

function wavFile(): File {
  // 44-byte WAV header + a little silence; the external mock ignores it.
  return new File([new Uint8Array(64)], "audio.wav", { type: "audio/wav" });
}

const externalSettings = {
  "stt.provider": "external",
  "stt.external.baseUrl": "https://api.example.com/v1",
  "stt.external.model": "whisper-1",
  "stt.external.apiKey": "sk-real",
};

Deno.test("transcribeAudio: external provider posts to the OpenAI transcriptions endpoint", async () => {
  const env = await setupTestEnv();
  const realFetch = globalThis.fetch;
  try {
    let calledUrl = "";
    globalThis.fetch = ((input: string | URL | Request) => {
      calledUrl = typeof input === "string" ? input : input.toString();
      return Promise.resolve(
        new Response(JSON.stringify({ text: "hello world" }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      );
    }) as typeof fetch;

    const text = await transcribeAudio(externalSettings, wavFile(), undefined);
    assertEquals(text, "hello world");
    assertEquals(calledUrl.includes("/audio/transcriptions"), true);
  } finally {
    globalThis.fetch = realFetch;
    await env.teardown();
  }
});

Deno.test("transcribeAudio: external provider forwards the configured default language", async () => {
  const env = await setupTestEnv();
  const realFetch = globalThis.fetch;
  try {
    let sentLanguage: string | null = null;
    globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
      // The SDK sends multipart form-data; reconstruct a Request to read it.
      const form = await new Request(
        typeof input === "string" || input instanceof URL ? input : input.url,
        init ?? (input as Request),
      ).formData();
      sentLanguage = String(form.get("language") ?? "");
      return new Response(JSON.stringify({ text: "hola" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }) as typeof fetch;

    await transcribeAudio(
      { ...externalSettings, "stt.external.language": "es" },
      wavFile(),
      undefined,
    );
    assertEquals(sentLanguage, "es");
  } finally {
    globalThis.fetch = realFetch;
    await env.teardown();
  }
});

Deno.test("transcribeAudio: external provider without baseUrl/model is a validation error", async () => {
  const env = await setupTestEnv();
  try {
    const err = await assertRejects(
      () => transcribeAudio({ "stt.provider": "external" }, wavFile(), undefined),
      AppError,
    );
    assertEquals(err.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("transcribeAudio: external loopback gateway needs no API key", async () => {
  const env = await setupTestEnv();
  const realFetch = globalThis.fetch;
  try {
    globalThis.fetch = (() =>
      Promise.resolve(
        new Response(JSON.stringify({ text: "ok" }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      )) as typeof fetch;
    const text = await transcribeAudio(
      {
        "stt.provider": "external",
        "stt.external.baseUrl": "http://127.0.0.1:9000/v1",
        "stt.external.model": "whisper-1",
      },
      wavFile(),
      undefined,
    );
    assertEquals(text, "ok");
  } finally {
    globalThis.fetch = realFetch;
    await env.teardown();
  }
});

Deno.test("transcribeAudio: external provider without an API key is a validation error", async () => {
  const env = await setupTestEnv();
  try {
    const err = await assertRejects(
      () =>
        transcribeAudio(
          {
            "stt.provider": "external",
            "stt.external.baseUrl": "https://api.example.com/v1",
            "stt.external.model": "whisper-1",
          },
          wavFile(),
          undefined,
        ),
      AppError,
    );
    assertEquals(err.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("transcribeAudio: external 401 maps to provider_unauthorized", async () => {
  const env = await setupTestEnv();
  const realFetch = globalThis.fetch;
  try {
    globalThis.fetch = (() =>
      Promise.resolve(new Response("nope", { status: 401 }))) as typeof fetch;
    const err = await assertRejects(
      () => transcribeAudio(externalSettings, wavFile(), undefined),
      AppError,
    );
    assertEquals(err.code, "provider_unauthorized");
  } finally {
    globalThis.fetch = realFetch;
    await env.teardown();
  }
});
