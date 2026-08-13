// The external-provider embedding path: when llm is in external mode with a
// Relevance Model set, embeddings reuse that provider (any vector dimension);
// otherwise embeddings are unavailable and callers degrade. Also covers the
// active-model identity that drives the relevance staleness hash.

import { assertEquals, assertRejects } from "@std/assert";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { patchCoreSettings } from "@loca/core-engine/services/core-settings";
import {
  activeEmbedModelId,
  embed,
  initEmbeddingService,
  isEmbeddingModelReady,
} from "@loca/core-engine/services/embedding";
import { AppError } from "@loca/core-engine";

// Configure external llm + a Relevance Model, with a plaintext API key (the
// vault fallback resolveExternalApiKey honors for a non-loopback host).
async function configureExternalEmbed(embedModel = "text-embedding-3-small"): Promise<void> {
  await patchCoreSettings({
    "llm.provider": "external",
    "llm.external.baseUrl": "https://api.example.com/v1",
    "llm.external.apiKey": "sk-test-123",
    "llm.external.model": "gpt-4o-mini",
    "llm.external.embedModel": embedModel,
  });
}

// Swap globalThis.fetch for the duration of `fn`, restoring it after.
async function withFetch(
  impl: (input: string | URL | Request, init?: RequestInit) => Promise<Response>,
  fn: () => Promise<void>,
): Promise<void> {
  const original = globalThis.fetch;
  globalThis.fetch = impl as typeof fetch;
  try {
    await fn();
  } finally {
    globalThis.fetch = original;
  }
}

function embeddingsResponse(vectors: number[][]): Response {
  return new Response(
    JSON.stringify({ data: vectors.map((embedding, index) => ({ index, embedding })) }),
    { status: 200, headers: { "content-type": "application/json" } },
  );
}

Deno.test("embed: routes to the external provider and returns its (non-384) vectors", async () => {
  const env = await setupTestEnv();
  try {
    await configureExternalEmbed();
    let seenUrl = "";
    let seenAuth: string | null = null;
    let seenModel = "";
    await withFetch(
      async (input, init) => {
        seenUrl = String(input);
        seenAuth = new Headers(init?.headers).get("authorization");
        seenModel = JSON.parse(init?.body as string).model;
        // A 5-dim vector: the external model's dimension, not the local 384.
        return embeddingsResponse([[3, 4, 0, 0, 0]]);
      },
      async () => {
        const [vec] = await embed(["hello"]);
        assertEquals(vec.length, 5);
        // L2-normalized: 3-4-0-0-0 -> 0.6, 0.8, 0, 0, 0.
        assertEquals(Math.round(vec[0] * 100) / 100, 0.6);
        assertEquals(Math.round(vec[1] * 100) / 100, 0.8);
      },
    );
    assertEquals(seenUrl, "https://api.example.com/v1/embeddings");
    assertEquals(seenAuth, "Bearer sk-test-123");
    assertEquals(seenModel, "text-embedding-3-small");
  } finally {
    await env.teardown();
  }
});

Deno.test("embed: a provider error (non-2xx) rejects so callers degrade", async () => {
  const env = await setupTestEnv();
  try {
    await configureExternalEmbed();
    await withFetch(
      () => Promise.resolve(new Response("nope", { status: 404 })),
      async () => {
        await assertRejects(() => embed(["hi"]), AppError);
      },
    );
  } finally {
    await env.teardown();
  }
});

Deno.test("embed: external mode with no Relevance Model is unavailable (throws, not ready)", async () => {
  const env = await setupTestEnv();
  try {
    await configureExternalEmbed(""); // provider external, embedModel blank
    assertEquals(await isEmbeddingModelReady(), false);
    // Even if a request were somehow attempted, there is no endpoint to hit.
    await withFetch(
      () => Promise.reject(new Error("fetch must not be called")),
      async () => {
        await assertRejects(() => embed(["x"]), AppError, "not available");
      },
    );
  } finally {
    await env.teardown();
  }
});

Deno.test("activeEmbedModelId: tracks the configured Relevance Model (drives the staleness hash)", async () => {
  const env = await setupTestEnv();
  try {
    // Default (local provider): the local model identity.
    await initEmbeddingService();
    const localId = activeEmbedModelId();

    // Switch to an external Relevance Model: the identity changes, so the
    // relevance staleness hash changes and stored vectors re-embed.
    await configureExternalEmbed("text-embedding-3-large");
    assertEquals(activeEmbedModelId(), "text-embedding-3-large");
    assertEquals(activeEmbedModelId() !== localId, true);

    // Clearing it falls back to the local identity.
    await configureExternalEmbed("");
    assertEquals(activeEmbedModelId(), localId);
  } finally {
    await env.teardown();
  }
});
