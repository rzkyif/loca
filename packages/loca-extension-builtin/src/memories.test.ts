// Memory tools over a scripted ctx.memories (no real host bridge):
// argument validation, result kinds, and show_memory's markdown push.

import { assertEquals, assertRejects } from "@std/assert";
import { editMemory, listMemories, readMemory, showMemory, writeMemory } from "./memories.ts";
import type { ToolContext } from "./types.ts";

interface MockCtx extends ToolContext {
  /** Replay log: every memories call as [op, ...args]. */
  calls: string[][];
  /** Replay log: markdown pushed via ctx.display. */
  shown: string[];
}

function makeMockCtx(): MockCtx {
  const ctx = {
    calls: [],
    shown: [],
    setProgress() {},
    askUser: () => Promise.resolve([]),
    log() {},
    display: {
      markdown(markdown: string) {
        ctx.shown.push(markdown);
      },
      image() {},
      table() {},
      diff() {},
    },
    memories: {
      list() {
        ctx.calls.push(["list"]);
        return Promise.resolve([
          { title: "Notes", kind: "knowledge" as const, summary: "my notes", updatedAtMs: 1 },
          { title: "Web research", kind: "skill" as const, updatedAtMs: 2 },
        ]);
      },
      get(title: string) {
        ctx.calls.push(["get", title]);
        return Promise.resolve({ title, content: `# ${title}\nbody` });
      },
      getFile(title: string, name: string) {
        ctx.calls.push(["getFile", title, name]);
        return Promise.resolve({ title, name, content: `contents of ${name}` });
      },
      write(title: string, content: string) {
        ctx.calls.push(["write", title, content]);
        return Promise.resolve({
          title,
          before: "",
          after: content,
          created: true,
        });
      },
      edit(title: string, find: string, replace: string) {
        ctx.calls.push(["edit", title, find, replace]);
        return Promise.resolve({ title, before: find, after: replace });
      },
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
    getChatContext() {
      return { userMessage: "", sessionId: null };
    },
  } as MockCtx;
  return ctx;
}

Deno.test("list_memories: returns titles, kinds, and summaries", async () => {
  const ctx = makeMockCtx();
  const result = await listMemories({}, ctx);
  assertEquals(result, {
    memories: [
      { title: "Notes", kind: "knowledge", summary: "my notes" },
      { title: "Web research", kind: "skill", summary: undefined },
    ],
  });
  assertEquals(ctx.calls, [["list"]]);
});

Deno.test("write_memory: returns a memory_diff and allows empty content", async () => {
  const ctx = makeMockCtx();
  const result = await writeMemory({ title: "Notes", content: "" }, ctx);
  assertEquals(result, {
    kind: "memory_diff",
    title: "Notes",
    before: "",
    after: "",
    created: true,
  });
  assertEquals(ctx.calls, [["write", "Notes", ""]]);
  await assertRejects(() => writeMemory({ content: "x" }, ctx), Error, "title");
});

Deno.test("edit_memory: returns a memory_diff and requires non-empty find", async () => {
  const ctx = makeMockCtx();
  const result = await editMemory({ title: "Notes", find: "a", replace: "" }, ctx);
  assertEquals(result, {
    kind: "memory_diff",
    title: "Notes",
    before: "a",
    after: "",
  });
  await assertRejects(
    () => editMemory({ title: "Notes", find: " ", replace: "x" }, ctx),
    Error,
    "find",
  );
});

Deno.test("read_memory: returns a memory_content result", async () => {
  const ctx = makeMockCtx();
  const result = await readMemory({ title: "Notes" }, ctx);
  assertEquals(result, {
    kind: "memory_content",
    title: "Notes",
    content: "# Notes\nbody",
  });
});

Deno.test("show_memory: pushes the content as a markdown display", async () => {
  const ctx = makeMockCtx();
  const result = await showMemory({ title: "Notes" }, ctx);
  assertEquals(result, { title: "Notes", shown: true });
  assertEquals(ctx.shown, ["# Notes\nbody"]);
});
