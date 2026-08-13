// sample_memory over a scripted ctx.memories backed by a tiny in-memory
// map: that it lists existing titles, writes the new memory, and reads it
// back.

import { assertEquals } from "@std/assert";
import { sampleMemory } from "./capabilities.ts";
import type { MemoryListing, ToolContext } from "./types.ts";

interface MemCtx extends ToolContext {
  store: Map<string, string>;
  calls: string[][];
}

function makeCtx(seed: MemoryListing[]): MemCtx {
  const store = new Map<string, string>();
  const ctx = {
    store,
    calls: [],
    setProgress() {},
    askUser: () => Promise.resolve([]),
    log() {},
    display: { markdown() {}, image() {}, table() {}, diff() {} },
    memories: {
      list() {
        ctx.calls.push(["list"]);
        return Promise.resolve(seed);
      },
      get(title: string) {
        ctx.calls.push(["get", title]);
        return Promise.resolve({ title, content: store.get(title) ?? "" });
      },
      getFile: () => Promise.reject(new Error("not scripted")),
      write(title: string, content: string) {
        ctx.calls.push(["write", title, content]);
        const created = !store.has(title);
        store.set(title, content);
        return Promise.resolve({ title, before: "", after: content, created });
      },
      edit: () => Promise.reject(new Error("not scripted")),
    },
    db: {
      query: () => Promise.resolve([]),
      execute: () => Promise.resolve({ changes: 0, lastInsertRowId: 0 }),
    },
    llm: { complete: () => Promise.reject(new Error("not scripted")) },
    tts: { speak: () => Promise.reject(new Error("not scripted")) },
    stt: { transcribe: () => Promise.reject(new Error("not scripted")) },
    schedulePrompt: () => Promise.reject(new Error("not scripted")),
    signal: new AbortController().signal,
    getChatContext: () => ({ userMessage: "", sessionId: null }),
  } as MemCtx;
  return ctx;
}

Deno.test("sample_memory: lists, writes, and reads back the memory", async () => {
  const ctx = makeCtx([
    { title: "Notes", kind: "knowledge", updatedAtMs: 1 },
    { title: "Web research", kind: "skill", updatedAtMs: 2 },
  ]);
  const result = await sampleMemory({ title: "My note", content: "body" }, ctx);

  assertEquals(result, {
    titles: ["Notes", "Web research"],
    wrote: "My note",
    content: "body",
  });
  assertEquals(ctx.store.get("My note"), "body");
  assertEquals(ctx.calls, [["list"], ["write", "My note", "body"], ["get", "My note"]]);
});

Deno.test("sample_memory: applies the default title and content", async () => {
  const ctx = makeCtx([]);
  const result = await sampleMemory({}, ctx);
  assertEquals(result.wrote, "Sample note");
  assertEquals(result.titles, []);
  assertEquals(ctx.store.get("Sample note"), result.content);
});
