// fetch_webpage over a mocked fetch: readable-content extraction from HTML,
// plain-text passthrough for non-HTML responses, and URL validation.

import { assert, assertEquals, assertRejects, assertStringIncludes } from "@std/assert";
import { fetchWebpage } from "./webpage.ts";
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

function installFakeFetch(body: string, contentType: string, status = 200): () => void {
  const original = globalThis.fetch;
  globalThis.fetch = (() =>
    Promise.resolve(
      new Response(body, { status, headers: { "content-type": contentType } }),
    )) as typeof fetch;
  return () => {
    globalThis.fetch = original;
  };
}

Deno.test("fetch_webpage: extracts title and text from an HTML page", async () => {
  const para = "loca is a local-first modular AI client. ".repeat(30).trim();
  const html = `<html><head><title>About loca</title></head><body>
    <article><h1>About loca</h1><p>${para}</p></article>
    </body></html>`;
  const restore = installFakeFetch(html, "text/html");
  try {
    const r = await fetchWebpage({ url: "https://example.com/about" }, makeCtx());
    assertEquals(r.title, "About loca");
    assertStringIncludes(r.content, "local-first modular AI client");
    assert(!r.content.includes("<p>"), "content should be plain text, not HTML");
  } finally {
    restore();
  }
});

Deno.test("fetch_webpage: passes non-HTML responses through as text", async () => {
  const restore = installFakeFetch("plain body, no markup", "text/plain");
  try {
    const r = await fetchWebpage({ url: "https://example.com/raw.txt" }, makeCtx());
    assertEquals(r.title, "");
    assertEquals(r.content, "plain body, no markup");
    assertEquals(r.truncated, false);
  } finally {
    restore();
  }
});

Deno.test("fetch_webpage: rejects non-http(s) URLs before fetching", async () => {
  await assertRejects(
    () => fetchWebpage({ url: "file:///etc/passwd" }, makeCtx()),
    Error,
    "only http(s)",
  );
});

Deno.test("fetch_webpage: surfaces non-2xx statuses", async () => {
  const restore = installFakeFetch("gone", "text/html", 404);
  try {
    await assertRejects(
      () => fetchWebpage({ url: "https://example.com/missing" }, makeCtx()),
      Error,
      "server returned 404",
    );
  } finally {
    restore();
  }
});
