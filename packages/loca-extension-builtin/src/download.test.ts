// download tool. Mocks global fetch with a streamed in-memory body,
// points XDG_DOWNLOAD_DIR at a tempdir, and verifies progress + bytes +
// abort. Demonstrates the worked-example test pattern for extension
// authors who want to test their own network-touching tools.

import { assertEquals, assertRejects } from "@std/assert";
import { join } from "@std/path";
import { download } from "./download.ts";
import type { ToolContext } from "./types.ts";

interface RecordedCtx extends ToolContext {
  progress: Array<{ progress: number; label?: string; description?: string }>;
  logs: Array<{ level: string; message: string }>;
}

function makeCtx(signal?: AbortSignal): RecordedCtx {
  const progress: RecordedCtx["progress"] = [];
  const logs: RecordedCtx["logs"] = [];
  return {
    progress,
    logs,
    setProgress(p, label, description) {
      progress.push({ progress: p, label, description });
    },
    askUser: () => Promise.resolve([]),
    log(level, message) {
      logs.push({ level, message });
    },
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
    signal: signal ?? new AbortController().signal,
    getChatContext: () => ({ userMessage: "", sessionId: null }),
  };
}

function bodyStreamOf(payload: Uint8Array): ReadableStream<Uint8Array> {
  return new ReadableStream({
    start(controller) {
      controller.enqueue(payload);
      controller.close();
    },
  });
}

interface MockResponse {
  status?: number;
  headers?: Record<string, string>;
  body?: Uint8Array;
}

function installFakeFetch(plan: (url: string) => MockResponse): () => void {
  const original = globalThis.fetch;
  globalThis.fetch = ((input: RequestInfo | URL, _init?: RequestInit) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof URL
          ? input.toString()
          : (input as Request).url;
    const r = plan(url);
    const body = r.body ?? new Uint8Array(0);
    return Promise.resolve(
      new Response(bodyStreamOf(body), {
        status: r.status ?? 200,
        headers: {
          "content-length": String(body.byteLength),
          ...r.headers,
        },
      }),
    );
  }) as typeof fetch;
  return () => {
    globalThis.fetch = original;
  };
}

async function withTempDownloads<T>(fn: (dir: string) => Promise<T>): Promise<T> {
  const dir = await Deno.makeTempDir({ prefix: "loca-download-t1-" });
  const prior = Deno.env.get("XDG_DOWNLOAD_DIR");
  Deno.env.set("XDG_DOWNLOAD_DIR", dir);
  try {
    return await fn(dir);
  } finally {
    if (prior === undefined) Deno.env.delete("XDG_DOWNLOAD_DIR");
    else Deno.env.set("XDG_DOWNLOAD_DIR", prior);
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
}

Deno.test("download: writes the body to XDG_DOWNLOAD_DIR and reports bytes", async () => {
  await withTempDownloads(async (dir) => {
    const payload = new TextEncoder().encode("hello loca");
    const restore = installFakeFetch(() => ({ body: payload }));
    try {
      const ctx = makeCtx();
      const result = await download({ url: "https://example.com/file.txt" }, ctx);
      assertEquals(result.bytes, payload.byteLength);
      assertEquals(result.path.startsWith(dir), true);
      const written = await Deno.readFile(result.path);
      assertEquals(written, payload);
    } finally {
      restore();
    }
  });
});

Deno.test("download: setProgress fires 0 at start and 1 at completion", async () => {
  await withTempDownloads(async () => {
    const restore = installFakeFetch(() => ({
      body: new TextEncoder().encode("ok"),
    }));
    try {
      const ctx = makeCtx();
      await download({ url: "https://example.com/x" }, ctx);
      assertEquals(ctx.progress[0].progress, 0);
      assertEquals(ctx.progress.at(-1)?.progress, 1);
    } finally {
      restore();
    }
  });
});

Deno.test("download: rejects non-http(s) URLs before touching fetch", async () => {
  await withTempDownloads(async () => {
    const ctx = makeCtx();
    await assertRejects(() => download({ url: "ftp://example.com/x" }, ctx), Error, "only http(s)");
  });
});

Deno.test("download: throws when the server returns a non-2xx status", async () => {
  await withTempDownloads(async () => {
    const restore = installFakeFetch(() => ({ status: 404 }));
    try {
      const ctx = makeCtx();
      await assertRejects(
        () => download({ url: "https://example.com/x" }, ctx),
        Error,
        "server returned 404",
      );
    } finally {
      restore();
    }
  });
});

Deno.test("download: honors ctx.signal aborted before the first chunk", async () => {
  await withTempDownloads(async () => {
    const restore = installFakeFetch(() => ({
      body: new TextEncoder().encode("x"),
    }));
    try {
      const abort = new AbortController();
      abort.abort();
      const ctx = makeCtx(abort.signal);
      await assertRejects(() => download({ url: "https://example.com/x" }, ctx), Error);
    } finally {
      restore();
    }
  });
});

Deno.test("download: derives filename from URL path when no filename arg is given", async () => {
  await withTempDownloads(async (dir) => {
    const restore = installFakeFetch(() => ({
      body: new TextEncoder().encode("y"),
    }));
    try {
      const ctx = makeCtx();
      const result = await download(
        {
          url: "https://example.com/sub/MyFile.bin",
        },
        ctx,
      );
      assertEquals(result.path, join(dir, "MyFile.bin"));
    } finally {
      restore();
    }
  });
});
