// streamChatCompletion normalizes the OpenAI SSE wire format into our
// LlmDelta shape. Driven by recorded SSE fixtures via the injected fetch
// seam on LlmEndpointConfig. No real OpenAI calls.

import { assertEquals } from "@std/assert";
import { type LlmDelta, streamChatCompletion } from "@loca/core-engine/services/llm-provider";
import { fakeOpenAIFetch } from "../../tests/helpers/fake-openai.ts";

async function collect(iter: AsyncIterable<LlmDelta>): Promise<LlmDelta[]> {
  const out: LlmDelta[] = [];
  for await (const d of iter) out.push(d);
  return out;
}

// Wraps the SSE fixture fetch to capture the JSON request body the SDK sends,
// so we can assert which sampling / thinking params went on the wire.
function capturingFetch(fixtureName: string): {
  fetch: typeof fetch;
  body: () => Record<string, unknown>;
} {
  const inner = fakeOpenAIFetch(fixtureName);
  let captured: Record<string, unknown> | null = null;
  const capture = ((input: RequestInfo | URL, init?: RequestInit) => {
    if (typeof init?.body === "string") captured = JSON.parse(init.body);
    return inner(input, init);
  }) as typeof fetch;
  return {
    fetch: capture,
    body: () => {
      if (!captured) throw new Error("no request body captured");
      return captured;
    },
  };
}

Deno.test("streamChatCompletion: parses content deltas and finish + usage", async () => {
  const deltas = await collect(
    streamChatCompletion({
      endpoint: {
        baseUrl: "https://stub.test/v1",
        apiKey: "sk-test",
        model: "test",
        fetch: fakeOpenAIFetch("streaming-basic.sse"),
      },
      messages: [{ role: "user", content: "hi" }],
    }),
  );

  const text = deltas.map((d) => d.contentDelta ?? "").join("");
  assertEquals(text, "Hello world");
  const finish = deltas.find((d) => d.finishReason);
  assertEquals(finish?.finishReason, "stop");
  const usage = deltas.find((d) => d.usage);
  assertEquals(usage?.usage, { prompt: 10, completion: 3, total: 13 });
});

Deno.test("streamChatCompletion: surfaces reasoning_content as reasoningDelta", async () => {
  const deltas = await collect(
    streamChatCompletion({
      endpoint: {
        baseUrl: "https://stub.test/v1",
        apiKey: "sk-test",
        model: "test",
        fetch: fakeOpenAIFetch("streaming-reasoning-tools.sse"),
      },
      messages: [{ role: "user", content: "calc" }],
    }),
  );
  const reasoning = deltas.map((d) => d.reasoningDelta ?? "").join("");
  assertEquals(reasoning, "Let me think... about this.");
});

Deno.test("streamChatCompletion: assembles tool_calls index-by-index", async () => {
  const deltas = await collect(
    streamChatCompletion({
      endpoint: {
        baseUrl: "https://stub.test/v1",
        apiKey: "sk-test",
        model: "test",
        fetch: fakeOpenAIFetch("streaming-reasoning-tools.sse"),
      },
      messages: [{ role: "user", content: "calc" }],
    }),
  );
  const toolCallChunks = deltas.flatMap((d) => d.toolCalls ?? []);
  // Two deltas, both for index 0; arguments stream as fragments.
  assertEquals(toolCallChunks.length, 2);
  assertEquals(toolCallChunks[0].id, "call_1");
  assertEquals(toolCallChunks[0].name, "do_thing");
  const args = toolCallChunks.map((tc) => tc.argumentsDelta ?? "").join("");
  assertEquals(args, '{"x":42}');
  const finish = deltas.find((d) => d.finishReason);
  assertEquals(finish?.finishReason, "tool_calls");
});

Deno.test("streamChatCompletion: local request carries thinking budget + llama samplers", async () => {
  const cap = capturingFetch("streaming-basic.sse");
  await collect(
    streamChatCompletion({
      endpoint: {
        baseUrl: "http://127.0.0.1:7701/v1",
        apiKey: "sk-local",
        model: "default",
        reasoning: "on",
        temperature: 0.5,
        topP: 0.9,
        topK: 20,
        minP: 0.05,
        repeatPenalty: 1.1,
        dryMultiplier: 0.8,
        presencePenalty: 1.5,
        samplers: ["top_k", "temperature", "dry", "penalties"],
        reasoningBudget: 256,
        fetch: cap.fetch,
      },
      messages: [{ role: "user", content: "hi" }],
    }),
  );
  const body = cap.body();
  assertEquals(body.temperature, 0.5);
  assertEquals(body.top_p, 0.9);
  assertEquals(body.top_k, 20);
  assertEquals(body.min_p, 0.05);
  assertEquals(body.repeat_penalty, 1.1);
  assertEquals(body.dry_multiplier, 0.8);
  assertEquals(body.presence_penalty, 1.5);
  assertEquals(body.samplers, ["top_k", "temperature", "dry", "penalties"]);
  // llama-server reads the per-request thinking cap as `thinking_budget_tokens`
  // and only enforces it when `reasoning_control` is true (a top-level `reasoning_budget`
  // is silently dropped, so both must be present).
  assertEquals(body.thinking_budget_tokens, 256);
  assertEquals(body.reasoning_control, true);
  assertEquals(body.chat_template_kwargs, { enable_thinking: true });
});

Deno.test("streamChatCompletion: external request uses reasoning_effort, drops llama samplers", async () => {
  const cap = capturingFetch("streaming-basic.sse");
  await collect(
    streamChatCompletion({
      endpoint: {
        baseUrl: "https://stub.test/v1",
        apiKey: "sk-real",
        model: "gpt-4o-mini",
        reasoning: "on",
        temperature: 0.5,
        topP: 0.9,
        // Even if present, llama-only knobs must not reach an external endpoint.
        topK: 20,
        minP: 0.05,
        repeatPenalty: 1.1,
        fetch: cap.fetch,
      },
      messages: [{ role: "user", content: "hi" }],
    }),
  );
  const body = cap.body();
  assertEquals(body.temperature, 0.5);
  assertEquals(body.top_p, 0.9);
  assertEquals(body.reasoning_effort, "high");
  assertEquals(body.top_k, undefined);
  assertEquals(body.min_p, undefined);
  assertEquals(body.repeat_penalty, undefined);
  assertEquals(body.dry_multiplier, undefined);
  assertEquals(body.samplers, undefined);
  assertEquals(body.chat_template_kwargs, undefined);
  assertEquals(body.thinking_budget_tokens, undefined);
  assertEquals(body.reasoning_control, undefined);
});

Deno.test("streamChatCompletion: external request forwards reasoningEffort level", async () => {
  const cap = capturingFetch("streaming-basic.sse");
  await collect(
    streamChatCompletion({
      endpoint: {
        baseUrl: "https://stub.test/v1",
        apiKey: "sk-real",
        model: "gpt-4o-mini",
        reasoning: "on",
        reasoningEffort: "medium",
        fetch: cap.fetch,
      },
      messages: [{ role: "user", content: "hi" }],
    }),
  );
  assertEquals(cap.body().reasoning_effort, "medium");
});

Deno.test("streamChatCompletion: thinking off sends enable_thinking false (local)", async () => {
  const cap = capturingFetch("streaming-basic.sse");
  await collect(
    streamChatCompletion({
      endpoint: {
        baseUrl: "http://127.0.0.1:7701/v1",
        apiKey: "sk-local",
        model: "default",
        reasoning: "off",
        reasoningBudget: 256,
        fetch: cap.fetch,
      },
      messages: [{ role: "user", content: "hi" }],
    }),
  );
  const body = cap.body();
  assertEquals(body.chat_template_kwargs, { enable_thinking: false });
  // No budget is sent when thinking is off.
  assertEquals(body.thinking_budget_tokens, undefined);
  assertEquals(body.reasoning_control, undefined);
});
