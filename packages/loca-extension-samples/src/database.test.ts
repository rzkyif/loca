// sample_database over a recording ctx.db backed by a tiny in-memory map:
// that the upsert writes and the read-back returns the stored value.

import { assert, assertEquals } from "@std/assert";
import { sampleDatabase } from "./database.ts";
import type { ToolContext } from "./types.ts";

interface DbCtx extends ToolContext {
  store: Map<string, string>;
  executed: string[];
}

function makeCtx(): DbCtx {
  const store = new Map<string, string>();
  const ctx = {
    store,
    executed: [],
    setProgress() {},
    askUser: () => Promise.resolve([]),
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
      execute(sql: string, params?: Array<string | number | boolean | null>) {
        ctx.executed.push(sql);
        if (sql.includes("INSERT INTO sample_kv") && params) {
          store.set(String(params[0]), String(params[1]));
        }
        return Promise.resolve({ changes: 1, lastInsertRowId: 0 });
      },
      query(sql: string, params?: Array<string | number | boolean | null>) {
        if (sql.includes("SELECT") && params) {
          const key = String(params[0]);
          const value = store.get(key);
          return Promise.resolve(value === undefined ? [] : [{ key, value }]);
        }
        return Promise.resolve([]);
      },
    },
    llm: { complete: () => Promise.reject(new Error("not scripted")) },
    tts: { speak: () => Promise.reject(new Error("not scripted")) },
    stt: { transcribe: () => Promise.reject(new Error("not scripted")) },
    schedulePrompt: () => Promise.reject(new Error("not scripted")),
    signal: new AbortController().signal,
    getChatContext: () => ({ userMessage: "", sessionId: null }),
  } as DbCtx;
  return ctx;
}

Deno.test("sample_database: upserts and reads back the value", async () => {
  const ctx = makeCtx();
  const result = await sampleDatabase({ key: "city", value: "amsterdam" }, ctx);
  assertEquals(result, { key: "city", value: "amsterdam" });
  assertEquals(ctx.store.get("city"), "amsterdam");
  assert(ctx.executed.some((sql) => sql.includes("CREATE TABLE IF NOT EXISTS sample_kv")));
});

Deno.test("sample_database: applies defaults when args are missing", async () => {
  const ctx = makeCtx();
  const result = await sampleDatabase({}, ctx);
  assertEquals(result, { key: "greeting", value: "hello" });
});
