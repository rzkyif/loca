// calculator: expression evaluation, comparisons, and rejection of
// non-finite or unparseable input.

import { assertEquals, assertThrows } from "@std/assert";
import { calculator } from "./calculator.ts";
import type { ToolContext } from "./types.ts";

function makeCtx(): ToolContext {
  return {
    setProgress() {},
    askUser: () => Promise.resolve([]),
    log() {},
    display: { markdown() {}, image() {}, table() {}, diff() {} },
    memories: {
      list: () => Promise.resolve([]),
      get: () => Promise.reject(new Error("not scripted")),
      write: () => Promise.reject(new Error("not scripted")),
      edit: () => Promise.reject(new Error("not scripted")),
    },
    db: {
      query: () => Promise.reject(new Error("not scripted")),
      execute: () => Promise.reject(new Error("not scripted")),
    },
    llm: { complete: () => Promise.reject(new Error("not scripted")) },
    tts: { speak: () => Promise.reject(new Error("not scripted")) },
    stt: { transcribe: () => Promise.reject(new Error("not scripted")) },
    schedulePrompt: () => Promise.reject(new Error("not scripted")),
    signal: new AbortController().signal,
    getChatContext: () => ({ userMessage: "", sessionId: null }),
  };
}

Deno.test("calculator: evaluates arithmetic with precedence", () => {
  assertEquals(calculator({ expression: "2 + 3 * 4" }, makeCtx()).result, 14);
});

Deno.test("calculator: supports functions and the power operator", () => {
  assertEquals(calculator({ expression: "sqrt(16) + 2^3" }, makeCtx()).result, 12);
});

Deno.test("calculator: comparisons return booleans", () => {
  assertEquals(calculator({ expression: "10 > 3" }, makeCtx()).result, true);
});

Deno.test("calculator: rejects non-finite results", () => {
  assertThrows(() => calculator({ expression: "1 / 0" }, makeCtx()), Error, "finite");
});

Deno.test("calculator: rejects unparseable expressions", () => {
  assertThrows(() => calculator({ expression: "2 +* 3" }, makeCtx()), Error, "could not evaluate");
});

Deno.test("calculator: rejects a missing expression", () => {
  assertThrows(() => calculator({}, makeCtx()), Error, "required");
});
