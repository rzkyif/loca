import { assertEquals, assertRejects } from "@std/assert";
import { fetchWithTimeout, streamDownload } from "./net.ts";

// Swap globalThis.fetch for the duration of `fn`, always restoring it.
async function withFetch(stub: typeof fetch, fn: () => Promise<void>): Promise<void> {
  const real = globalThis.fetch;
  globalThis.fetch = stub;
  try {
    await fn();
  } finally {
    globalThis.fetch = real;
  }
}

function bodyOf(chunks: Uint8Array[]): ReadableStream<Uint8Array> {
  return new ReadableStream({
    start(controller) {
      for (const c of chunks) controller.enqueue(c);
      controller.close();
    },
  });
}

Deno.test("streamDownload delivers every chunk on the happy path", async () => {
  const chunks = [new Uint8Array([1, 2]), new Uint8Array([3, 4, 5])];
  await withFetch(
    () => Promise.resolve(new Response(bodyOf(chunks))),
    async () => {
      const got: number[] = [];
      await streamDownload("https://x/y", (c) => {
        got.push(...c);
      });
      assertEquals(got, [1, 2, 3, 4, 5]);
    },
  );
});

Deno.test("streamDownload aborts when the connection stalls mid-stream", async () => {
  // A body that emits one chunk then never sends another and never closes. Real
  // fetch errors the body stream when its signal aborts; mirror that so the
  // for-await unblocks. The stall timer must fire rather than hang forever.
  await withFetch(
    (_url, init) => {
      const signal = (init as RequestInit).signal!;
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new Uint8Array([1]));
          signal.addEventListener("abort", () => {
            controller.error(new DOMException("aborted", "AbortError"));
          });
        },
      });
      return Promise.resolve(new Response(body));
    },
    async () => {
      const err = await assertRejects(() =>
        streamDownload("https://x/y", () => {}, { stallMs: 40 }),
      );
      assertEquals(String(err).includes("stalled"), true);
    },
  );
});

Deno.test("fetchWithTimeout rejects when the server never responds", async () => {
  await withFetch(
    (_url, init) => {
      const signal = (init as RequestInit).signal!;
      return new Promise<Response>((_resolve, reject) => {
        signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
      });
    },
    async () => {
      const err = await assertRejects(() => fetchWithTimeout("https://x/y", {}, 40));
      assertEquals(String(err).includes("timed out"), true);
    },
  );
});

Deno.test("fetchWithTimeout returns the response when it arrives in time", async () => {
  await withFetch(
    () => Promise.resolve(new Response("ok")),
    async () => {
      const res = await fetchWithTimeout("https://x/y", {}, 1000);
      assertEquals(await res.text(), "ok");
    },
  );
});
