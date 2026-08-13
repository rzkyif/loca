// sample_choice and sample_table over a scripted askUser and a recording
// ctx.db: that answers map to the returned shape and that table rows are
// persisted via ctx.db.execute.

import { assert, assertEquals } from "@std/assert";
import { sampleChoice, sampleTable } from "./ask.ts";
import type { AskUserAnswer, AskUserQuestion, ToolContext } from "./types.ts";

interface ScriptedCtx extends ToolContext {
  /** Every askUser call's questions, in order. */
  asked: AskUserQuestion[][];
  /** Recorded [sql, params] for each db.execute call. */
  executed: Array<[string, Array<string | number | boolean | null> | undefined]>;
}

function makeCtx(answers: AskUserAnswer[][]): ScriptedCtx {
  const queue = [...answers];
  const ctx = {
    asked: [],
    executed: [],
    setProgress() {},
    askUser(questions: AskUserQuestion[]) {
      ctx.asked.push(questions);
      const next = queue.shift();
      if (!next) {
        return Promise.reject(new Error("askUser called more times than scripted"));
      }
      return Promise.resolve(next);
    },
    log() {},
    display: { markdown() {}, image() {}, table() {}, diff() {} },
    memories: {
      list: () => Promise.resolve([]),
      get: () => Promise.reject(new Error("not scripted")),
      getFile: () => Promise.reject(new Error("not scripted")),
      write: () => Promise.reject(new Error("not scripted")),
      edit: () => Promise.reject(new Error("not scripted")),
    },
    db: {
      query: () => Promise.resolve([]),
      execute(sql: string, params?: Array<string | number | boolean | null>) {
        ctx.executed.push([sql, params]);
        return Promise.resolve({
          changes: 1,
          lastInsertRowId: ctx.executed.length,
        });
      },
    },
    llm: { complete: () => Promise.reject(new Error("not scripted")) },
    tts: { speak: () => Promise.reject(new Error("not scripted")) },
    stt: { transcribe: () => Promise.reject(new Error("not scripted")) },
    schedulePrompt: () => Promise.reject(new Error("not scripted")),
    signal: new AbortController().signal,
    getChatContext: () => ({ userMessage: "", sessionId: null }),
  } as ScriptedCtx;
  return ctx;
}

Deno.test("sample_choice: maps the three answers to name/color/preferences", async () => {
  const ctx = makeCtx([["Ada", "blue", ["dark", "tea"]]]);
  const result = await sampleChoice({}, ctx);
  assertEquals(result, {
    name: "Ada",
    color: "blue",
    preferences: ["dark", "tea"],
  });
  // One askUser call carrying the three questions in order.
  assertEquals(ctx.asked.length, 1);
  assertEquals(ctx.asked[0].length, 3);
});

Deno.test("sample_choice: tolerates empty and wrong-typed answers", async () => {
  const ctx = makeCtx([[[], "green", "compact"]]);
  const result = await sampleChoice({}, ctx);
  assertEquals(result, { name: "", color: "green", preferences: ["compact"] });
});

Deno.test("sample_table: persists each accepted row via ctx.db", async () => {
  const ctx = makeCtx([
    [
      [
        { item: "Apples", amount: "3" },
        {
          item: "Bananas",
          amount: "6",
        },
      ],
    ],
  ]);
  const result = await sampleTable({}, ctx);

  assertEquals(result.saved, 2);
  assertEquals(result.rows.length, 2);

  // One CREATE TABLE plus one INSERT per row.
  assert(ctx.executed[0][0].includes("CREATE TABLE IF NOT EXISTS sample_rows"));
  const inserts = ctx.executed.filter(([sql]) => sql.includes("INSERT INTO sample_rows"));
  assertEquals(inserts.length, 2);
  assertEquals(inserts[0][1]?.[0], JSON.stringify({ item: "Apples", amount: "3" }));
});

Deno.test("sample_table: honors custom columns and saves nothing on an empty answer", async () => {
  const ctx = makeCtx([[[]]]);
  const result = await sampleTable({ columns: ["name", "qty"] }, ctx);

  assertEquals(result, { saved: 0, rows: [] });
  const tableQ = ctx.asked[0][0];
  assertEquals(tableQ.kind, "table");
  if (tableQ.kind === "table") assertEquals(tableQ.columns, ["name", "qty"]);
  // Only the CREATE TABLE ran; no inserts.
  assertEquals(ctx.executed.filter(([sql]) => sql.includes("INSERT")).length, 0);
});
