// get_datetime: shape and consistency of the reported local time.

import { assert, assertEquals, assertMatch } from "@std/assert";
import { getDatetime } from "./datetime.ts";
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

const WEEKDAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

Deno.test("get_datetime: reports a consistent local now", () => {
  const before = Date.now();
  const r = getDatetime({}, makeCtx());
  const after = Date.now();

  assertMatch(r.date, /^\d{4}-\d{2}-\d{2}$/);
  assertMatch(r.time, /^\d{2}:\d{2}:\d{2}$/);
  assertMatch(r.iso, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}[+-]\d{2}:\d{2}$/);
  assert(WEEKDAYS.includes(r.weekday), `unexpected weekday ${r.weekday}`);
  assert(r.epochMs >= before && r.epochMs <= after);
  assertEquals(r.utcOffsetMinutes, -new Date(r.epochMs).getTimezoneOffset());
  // The offset-carrying ISO string parses back to the same instant
  // (epochMs keeps sub-second precision the string drops).
  const parsed = new Date(r.iso).getTime();
  assert(Math.abs(parsed - r.epochMs) < 1000);
});
