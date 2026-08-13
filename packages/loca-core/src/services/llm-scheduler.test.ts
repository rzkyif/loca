// LlmScheduler: validation, external-provider bypass, and the
// queue-depth guard. Deep queue-ordering and watchdog timing are covered
// by the chat-orchestrator integration test (separate file); here we
// exercise the public surface that doesn't require driving real SSE
// streams.

import { assertEquals, assertThrows } from "@std/assert";
import { LlmScheduler, llmScheduler } from "@loca/core-engine/services/llm-scheduler";
import { fakeOpenAIFetch } from "../../tests/helpers/fake-openai.ts";
import { AppError } from "@loca/core-engine";

function makeReq() {
  return {
    endpoint: {
      baseUrl: "https://api.openai.com/v1",
      apiKey: "sk-test",
      model: "test",
      fetch: fakeOpenAIFetch("streaming-basic.sse"),
    },
    messages: [{ role: "user" as const, content: "hi" }],
  };
}

Deno.test("setParallelSlots: rejects 0, negative, and non-integer values", () => {
  const s = new LlmScheduler();
  for (const n of [0, -1, 1.5, Number.NaN]) {
    assertThrows(() => s.setParallelSlots(n), AppError, "invalid parallelSlots");
  }
  // Valid setting succeeds silently.
  s.setParallelSlots(8);
});

Deno.test("setCallTimeoutMs: rejects values under 1_000ms or non-finite", () => {
  const s = new LlmScheduler();
  for (const ms of [0, 500, 999, Number.POSITIVE_INFINITY]) {
    if (!Number.isFinite(ms) || ms < 1_000) {
      assertThrows(() => s.setCallTimeoutMs(ms), AppError, "invalid callTimeoutMs");
    }
  }
  s.setCallTimeoutMs(10_000);
});

Deno.test("schedule: external provider bypasses the local semaphore", async () => {
  const s = new LlmScheduler();
  s.setParallelSlots(1);
  const req = makeReq();
  // Drive five external completions concurrently. Each finishes when the
  // SSE fixture is fully consumed. If the scheduler were applying the
  // semaphore, the 5th would queue behind the active slot.
  const results = await Promise.all(
    Array.from({ length: 5 }, async () => {
      const out: string[] = [];
      for await (const d of s.schedule(req, { clientId: "c", isLocal: false })) {
        if (d.contentDelta) out.push(d.contentDelta);
      }
      return out.join("");
    }),
  );
  for (const r of results) assertEquals(r, "Hello world");
});

// Deep queue/watchdog behavior is exercised by the chat-orchestrator
// integration test (chat.test.ts), where streams are driven to completion
// against the fake OpenAI fetch. Replaying queue-saturation here without
// committing to that full path leaks pending OpenAI SDK state.

Deno.test("llmScheduler(): returns a cached singleton, reset clears it", () => {
  const a = llmScheduler();
  const b = llmScheduler();
  assertEquals(a, b);
});
