// web_search over a mocked DuckDuckGo HTML page: result parsing, redirect
// unwrapping, the maxResults clamp, and the rate-limit error.

import { assertEquals, assertRejects } from "@std/assert";
import { webSearch } from "./search.ts";
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

function installFakeFetch(html: string, status = 200): () => void {
  const original = globalThis.fetch;
  globalThis.fetch = (() =>
    Promise.resolve(
      new Response(html, { status, headers: { "content-type": "text/html" } }),
    )) as typeof fetch;
  return () => {
    globalThis.fetch = original;
  };
}

function ddgResult(n: number): string {
  const target = encodeURIComponent(`https://example.com/page-${n}`);
  return `<div class="result">
    <h2 class="result__title">
      <a class="result__a" href="//duckduckgo.com/l/?uddg=${target}&amp;rut=abc${n}">Result ${n}</a>
    </h2>
    <a class="result__snippet">Snippet for result ${n}</a>
  </div>`;
}

function ddgPage(count: number): string {
  const items = Array.from({ length: count }, (_, i) => ddgResult(i + 1)).join("\n");
  return `<html><body><div class="results">${items}</div></body></html>`;
}

Deno.test("web_search: parses titles, unwraps redirect URLs, keeps snippets", async () => {
  const restore = installFakeFetch(ddgPage(3));
  try {
    const { results } = await webSearch({ query: "loca" }, makeCtx());
    assertEquals(results.length, 3);
    assertEquals(results[0], {
      title: "Result 1",
      url: "https://example.com/page-1",
      snippet: "Snippet for result 1",
    });
  } finally {
    restore();
  }
});

Deno.test("web_search: caps results at maxResults (default 5)", async () => {
  const restore = installFakeFetch(ddgPage(8));
  try {
    const byDefault = await webSearch({ query: "loca" }, makeCtx());
    assertEquals(byDefault.results.length, 5);
    const byArg = await webSearch({ query: "loca", maxResults: 2 }, makeCtx());
    assertEquals(byArg.results.length, 2);
  } finally {
    restore();
  }
});

Deno.test("web_search: rejects an empty query before fetching", async () => {
  await assertRejects(() => webSearch({ query: "  " }, makeCtx()), Error, "query is required");
});

Deno.test("web_search: reports DDG rate limiting as a clear error", async () => {
  const restore = installFakeFetch("<html><body><div class='anomaly-modal'></div></body></html>");
  try {
    await assertRejects(() => webSearch({ query: "loca" }, makeCtx()), Error, "rate-limiting");
  } finally {
    restore();
  }
});
