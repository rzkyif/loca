// singleShot is a thin loop over streamChatCompletion. The interesting
// behavior is "drains contentDelta, ignores everything else, trims, and
// returns a single string." Use a fake fetch to make the SDK emit a
// canned event-stream so we don't talk to a real LLM.

import { assertEquals } from "@std/assert";
import { singleShot } from "@loca/core-engine/services/single-shot";

// Build a minimal SSE stream the OpenAI SDK can parse into chat-completion
// chunks. Each chunk's `delta.content` becomes a contentDelta in our
// streamChatCompletion adapter.
function fakeSse(chunks: string[]): Response {
  const lines = chunks
    .map(
      (c) =>
        `data: ${JSON.stringify({
          choices: [{ delta: { content: c }, index: 0, finish_reason: null }],
        })}\n\n`,
    )
    .concat("data: [DONE]\n\n");
  const body = new TextEncoder().encode(lines.join(""));
  return new Response(body, {
    status: 200,
    headers: { "content-type": "text/event-stream" },
  });
}

Deno.test("singleShot: concatenates content deltas and trims the result", async () => {
  const fakeFetch = () => Promise.resolve(fakeSse(["  Hello", ", ", "world!  "]));
  const out = await singleShot({
    systemPrompt: "You are terse.",
    userMessage: "Greet me",
    endpoint: {
      baseUrl: "http://localhost:11434",
      apiKey: "test",
      model: "test-model",
      fetch: fakeFetch as unknown as (
        input: string | URL | Request,
        init?: RequestInit,
      ) => Promise<Response>,
    },
  });
  assertEquals(out, "Hello, world!");
});

Deno.test("singleShot: returns empty string when no content deltas arrive", async () => {
  const fakeFetch = () => Promise.resolve(fakeSse([]));
  const out = await singleShot({
    systemPrompt: "irrelevant",
    userMessage: "irrelevant",
    endpoint: {
      baseUrl: "http://localhost:11434",
      apiKey: "test",
      model: "test-model",
      fetch: fakeFetch as unknown as (
        input: string | URL | Request,
        init?: RequestInit,
      ) => Promise<Response>,
    },
  });
  assertEquals(out, "");
});
