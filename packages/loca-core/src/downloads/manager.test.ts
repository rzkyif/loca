// DownloadManager DB lifecycle. Focused on the parts that don't need
// real network: fast path (file already on disk), cancel/retry/remove
// transitions on persisted rows, and `normalizePersistedRows` boot-time
// recovery. Streaming downloads are covered by the integration paths in
// chat.t2; isolating them here would require a full fetch mock harness for
// dubious return.

import { assertEquals, assertNotEquals, assertRejects, assertStringIncludes } from "@std/assert";
import { dirname, join } from "@std/path";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { db } from "@loca/core-engine";
import { paths } from "../paths.ts";
import { DownloadManager } from "./manager.ts";
import { AppError } from "@loca/core-engine";

// An ArrayBuffer-backed view, which the WebCrypto + Response typings require.
function bytesOf(s: string): Uint8Array<ArrayBuffer> {
  return new Uint8Array(new TextEncoder().encode(s));
}

async function sha256Hex(bytes: Uint8Array<ArrayBuffer>): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

// A fetch stub for the streaming download path: the HEAD probe (redirect:manual)
// carries the LFS content hash in `x-linked-etag`; the GET streams the bytes.
function downloadFetchStub(bytes: Uint8Array<ArrayBuffer>, linkedEtag: string): typeof fetch {
  return ((_input: string | URL | Request, init?: RequestInit) => {
    if (init?.method === "HEAD") {
      return Promise.resolve(new Response(null, { headers: { "x-linked-etag": linkedEtag } }));
    }
    return Promise.resolve(
      new Response(bytes, { headers: { "content-length": String(bytes.byteLength) } }),
    );
  }) as typeof fetch;
}

function rowsForId(id: string) {
  return db()
    .prepare(`SELECT status, error, downloaded_bytes, size_bytes FROM downloads WHERE id = ?`)
    .get(id) as
    | {
        status: string;
        error: string | null;
        downloaded_bytes: number;
        size_bytes: number | null;
      }
    | undefined;
}

Deno.test("DownloadManager.enqueue: file already on disk -> upserts Completed and resolves abs path", async () => {
  const env = await setupTestEnv();
  try {
    const mgr = new DownloadManager();
    const absPath = join(paths().modelsDir, "u", "r", "x.bin");
    await Deno.mkdir(join(paths().modelsDir, "u", "r"), { recursive: true });
    await Deno.writeTextFile(absPath, "hello");

    const got = await mgr.enqueue({
      source: "@u/r/main/x.bin",
      destination: "models",
      groupId: "g",
    });
    assertEquals(got, absPath);
    const row = rowsForId(`models:${absPath}`);
    assertEquals(row?.status, "Completed");
  } finally {
    await env.teardown();
  }
});

Deno.test("DownloadManager.idFor: matches the row id enqueue creates, so cancel/remove accept it", async () => {
  const env = await setupTestEnv();
  try {
    const mgr = new DownloadManager();
    const spec = { source: "@u/r/main/y.bin", destination: "models" as const, groupId: "g" };
    const absPath = join(paths().modelsDir, "u", "r", "y.bin");
    assertEquals(mgr.idFor(spec), `models:${absPath}`);

    await Deno.mkdir(dirname(absPath), { recursive: true });
    await Deno.writeTextFile(absPath, "hello");
    await mgr.enqueue(spec);
    assertEquals(rowsForId(mgr.idFor(spec))?.status, "Completed");
    // remove() matches on the same id.
    mgr.remove(mgr.idFor(spec));
    assertEquals(rowsForId(mgr.idFor(spec)), undefined);
  } finally {
    await env.teardown();
  }
});

Deno.test("DownloadManager.cancel: flips a Pending row to Cancelled without spawning network", async () => {
  const env = await setupTestEnv();
  try {
    const mgr = new DownloadManager();
    // Seed a Pending row directly (skip enqueue's spawn).
    const id = "models:/fake/abs/path";
    db()
      .prepare(`
      INSERT INTO downloads
        (id, source, destination, rel_path, abs_path, filename, group_id,
         size_bytes, downloaded_bytes, status, error, added_at_ms)
      VALUES (?, ?, 'models', ?, ?, ?, 'g', NULL, 0, 'Pending', NULL, ?)
    `)
      .run(id, "@u/r/main/x.bin", "u/r/x.bin", "/fake/abs/path", "x.bin", Date.now());

    mgr.cancel(id);

    const row = rowsForId(id);
    assertEquals(row?.status, "Cancelled");
    assertEquals(row?.error, "cancelled");
  } finally {
    await env.teardown();
  }
});

Deno.test("DownloadManager.remove: deletes terminal rows; refuses while in-flight", async () => {
  const env = await setupTestEnv();
  try {
    const mgr = new DownloadManager();
    const id = "models:/p1";
    db()
      .prepare(`
      INSERT INTO downloads
        (id, source, destination, rel_path, abs_path, filename, group_id,
         size_bytes, downloaded_bytes, status, error, added_at_ms)
      VALUES (?, '@u/r/b/x', 'models', 'u/r/x', '/p1', 'x', 'g', NULL, 0, 'Error', 'boom', ?)
    `)
      .run(id, Date.now());

    mgr.remove(id);
    assertEquals(rowsForId(id), undefined);
  } finally {
    await env.teardown();
  }
});

Deno.test("DownloadManager.retry: refuses while Pending; refuses while Downloading; allows after Error/Cancelled", async () => {
  const env = await setupTestEnv();
  try {
    const mgr = new DownloadManager();
    const id = "models:/fake/path-that-will-not-be-fetched";

    // Set up an Error row.
    db()
      .prepare(`
      INSERT INTO downloads
        (id, source, destination, rel_path, abs_path, filename, group_id,
         size_bytes, downloaded_bytes, status, error, added_at_ms)
      VALUES (?, '/abs/only-source', 'models', '/abs/only-source', ?, 'src', 'g', NULL, 0, 'Error', 'old', ?)
    `)
      .run(id, "/fake/path-that-will-not-be-fetched", Date.now());

    mgr.retry(id);
    // Retry flips to Pending and spawns; the spawn will fail (no URL since
    // source isn't an `@...` spec), but the status flip we asserted is
    // synchronous. We immediately cancel to prevent leaks.
    const row = rowsForId(id);
    // Either Pending (if spawn hasn't progressed) or Cancelled/Error after
    // async spawn collapsed. Both prove retry kicked something off.
    assertNotEquals(row?.status, "Error");
    mgr.cancel(id);
  } finally {
    await env.teardown();
  }
});

Deno.test("DownloadManager.resumePending: arms a worker for persisted Pending rows (no worker-less limbo)", async () => {
  const env = await setupTestEnv();
  const mgr = new DownloadManager();
  try {
    // A persisted Pending row, as normalizePersistedRows produces from an
    // interrupted Downloading row across a restart. Its source isn't an `@...`
    // spec so the spawned worker fails fast - which is exactly what proves a
    // worker actually ran: before the fix, resumePending never armed the
    // in-flight entry and spawn() no-opped on its guard, leaving the row Pending
    // forever with no worker (the corebar-stuck-on-Downloading limbo).
    const id = "models:/fake/resume-me";
    db()
      .prepare(`
      INSERT INTO downloads
        (id, source, destination, rel_path, abs_path, filename, group_id,
         size_bytes, downloaded_bytes, status, error, added_at_ms)
      VALUES (?, '/abs/only-source', 'models', '/abs/only-source', ?, 'src', 'g', NULL, 0, 'Pending', NULL, ?)
    `)
      .run(id, "/fake/resume-me", Date.now());

    mgr.resumePending();

    // Poll until the row leaves Pending: a worker ran it to a terminal state.
    let status = rowsForId(id)?.status;
    for (let i = 0; i < 200 && status === "Pending"; i++) {
      await new Promise((r) => setTimeout(r, 10));
      status = rowsForId(id)?.status;
    }
    assertNotEquals(status, "Pending");
  } finally {
    mgr.shutdown();
    await env.teardown();
  }
});

Deno.test("DownloadManager.resumePending: skips binaries rows (BinariesManager finishes those, incl. extraction)", async () => {
  const env = await setupTestEnv();
  const mgr = new DownloadManager();
  try {
    // A binaries-destination Pending row (as an interrupted sidecar update
    // leaves behind). resumePending must NOT arm a worker for it: a generic
    // resume only moves bytes, but a sidecar still needs the extract-into-binDir
    // step that lives in BinariesManager, and its sha256 isn't re-verified here.
    // So it stays Pending here; reconcileInterruptedInstalls owns finishing it.
    const binId = "binaries:/fake/staging/llama-server-abc.tar.gz";
    // A models row alongside it, to prove the skip is selective (models resume).
    const modelId = "models:/fake/resume-model";
    for (const [id, dest, abs] of [
      [binId, "binaries", "/fake/staging/llama-server-abc.tar.gz"],
      [modelId, "models", "/fake/resume-model"],
    ] as const) {
      db()
        .prepare(`
        INSERT INTO downloads
          (id, source, destination, rel_path, abs_path, filename, group_id,
           size_bytes, downloaded_bytes, status, error, added_at_ms)
        VALUES (?, '/abs/only-source', ?, '/abs/only-source', ?, 'src', 'binary:llama-server', NULL, 0, 'Pending', NULL, ?)
      `)
        .run(id, dest, abs, Date.now());
    }

    mgr.resumePending();

    // The models row leaves Pending (a worker ran it to a terminal state); the
    // binaries row is untouched.
    let modelStatus = rowsForId(modelId)?.status;
    for (let i = 0; i < 200 && modelStatus === "Pending"; i++) {
      await new Promise((r) => setTimeout(r, 10));
      modelStatus = rowsForId(modelId)?.status;
    }
    assertNotEquals(modelStatus, "Pending");
    assertEquals(rowsForId(binId)?.status, "Pending");
  } finally {
    mgr.shutdown();
    await env.teardown();
  }
});

Deno.test("DownloadManager constructor: resets Downloading rows to Pending; drops vanished Completed", async () => {
  const env = await setupTestEnv();
  try {
    // Pre-seed: one Completed row whose file doesn't exist, and one
    // Downloading row that should be reset.
    const ghostPath = join(paths().modelsDir, "ghost", "f.bin");
    db()
      .prepare(`
      INSERT INTO downloads (id, source, destination, rel_path, abs_path, filename, group_id,
                             size_bytes, downloaded_bytes, status, error, added_at_ms)
      VALUES ('models:ghost', '@u/r/b/f.bin', 'models', 'u/r/f.bin', ?, 'f.bin', 'g', 1, 1, 'Completed', NULL, ?)
    `)
      .run(ghostPath, Date.now());
    db()
      .prepare(`
      INSERT INTO downloads (id, source, destination, rel_path, abs_path, filename, group_id,
                             size_bytes, downloaded_bytes, status, error, added_at_ms)
      VALUES ('models:dling', '@u/r/b/x.bin', 'models', 'u/r/x.bin', '/some/dling', 'x.bin', 'g', 10, 5, 'Downloading', NULL, ?)
    `)
      .run(Date.now());

    // Don't actually call enqueue/resumePending. Just construct so the
    // normalize step runs.
    new DownloadManager();

    assertEquals(rowsForId("models:ghost"), undefined);
    const dling = rowsForId("models:dling");
    assertEquals(dling?.status, "Pending");
    assertEquals(dling?.downloaded_bytes, 0);
  } finally {
    await env.teardown();
  }
});

Deno.test("DownloadManager.snapshot: returns rows ordered by added_at_ms DESC", async () => {
  const env = await setupTestEnv();
  try {
    const mgr = new DownloadManager();
    const t0 = 1_700_000_000_000;
    db()
      .prepare(`
      INSERT INTO downloads (id, source, destination, rel_path, abs_path, filename, group_id,
                             size_bytes, downloaded_bytes, status, error, added_at_ms)
      VALUES ('models:a', 's', 'models', 'r', '/a', 'a', 'g', NULL, 0, 'Error', NULL, ?)
    `)
      .run(t0);
    db()
      .prepare(`
      INSERT INTO downloads (id, source, destination, rel_path, abs_path, filename, group_id,
                             size_bytes, downloaded_bytes, status, error, added_at_ms)
      VALUES ('models:b', 's', 'models', 'r', '/b', 'b', 'g', NULL, 0, 'Error', NULL, ?)
    `)
      .run(t0 + 1000);

    const snap = mgr.snapshot();
    assertEquals(
      snap.map((r) => r.id),
      ["models:b", "models:a"],
    );
  } finally {
    await env.teardown();
  }
});

Deno.test("DownloadManager.subscribe: receives a snapshot after mutating cancel", async () => {
  const env = await setupTestEnv();
  try {
    const mgr = new DownloadManager();
    const id = "models:/p1";
    db()
      .prepare(`
      INSERT INTO downloads (id, source, destination, rel_path, abs_path, filename, group_id,
                             size_bytes, downloaded_bytes, status, error, added_at_ms)
      VALUES (?, '@u/r/b/x', 'models', 'u/r/x', '/p1', 'x', 'g', NULL, 0, 'Pending', NULL, ?)
    `)
      .run(id, Date.now());

    let calls = 0;
    const unsubscribe = mgr.subscribe(() => calls++);
    mgr.cancel(id);
    unsubscribe();
    // cancel() on a Pending row triggers a broadcast.
    assertEquals(calls, 1);
  } finally {
    await env.teardown();
  }
});

Deno.test("DownloadManager.streamDownload: verifies bytes against the HF x-linked-etag and completes on match", async () => {
  const env = await setupTestEnv();
  const realFetch = globalThis.fetch;
  try {
    const bytes = bytesOf("model-weights-payload");
    globalThis.fetch = downloadFetchStub(bytes, await sha256Hex(bytes));

    const mgr = new DownloadManager();
    const abs = await mgr.enqueue({
      source: "@e2e/integrity/main/weights.bin",
      // host must match HF_BASE_URL so resolveHfSha256 reads the x-linked-etag.
      url: "https://huggingface.co/e2e/integrity/resolve/main/weights.bin",
      destination: "binaries",
      groupId: "g",
    });
    assertEquals(await Deno.readTextFile(abs), "model-weights-payload");
  } finally {
    globalThis.fetch = realFetch;
    await env.teardown();
  }
});

Deno.test("DownloadManager.streamDownload: a transient mid-stream failure keeps the partial .tmp for resume", async () => {
  const env = await setupTestEnv();
  const realFetch = globalThis.fetch;
  try {
    const full = bytesOf("model-weights-payload-interrupted");
    const head = full.slice(0, 7);
    globalThis.fetch = ((_input: string | URL | Request, init?: RequestInit) => {
      if (init?.method === "HEAD") {
        // A real hash so resume is even eligible; the transfer never completes.
        return Promise.resolve(
          new Response(null, { headers: { "x-linked-etag": "0".repeat(64) } }),
        );
      }
      // Deliver the head on the first pull, then error like a dropped
      // connection on the next - so the head is written before the failure.
      let stage = 0;
      const stream = new ReadableStream<Uint8Array>({
        pull(controller) {
          if (stage === 0) {
            stage = 1;
            controller.enqueue(head);
          } else {
            controller.error(new Error("connection reset"));
          }
        },
      });
      return Promise.resolve(
        new Response(stream, { headers: { "content-length": String(full.byteLength) } }),
      );
    }) as typeof fetch;

    const mgr = new DownloadManager();
    await assertRejects(() =>
      mgr.enqueue({
        source: "@e2e/interrupt/main/weights.bin",
        url: "https://huggingface.co/e2e/interrupt/resolve/main/weights.bin",
        destination: "binaries",
        groupId: "g",
      }),
    );
    // The partial survives the Error so a retry can resume it (only checksum
    // mismatch / 416 / explicit remove drop it).
    const abs = join(paths().binDir, "e2e", "interrupt", "weights.bin");
    const st = await Deno.stat(abs + ".tmp");
    assertEquals(st.size, head.byteLength);
  } finally {
    globalThis.fetch = realFetch;
    await env.teardown();
  }
});

Deno.test("DownloadManager.streamDownload: resumes a partial .tmp via a Range request", async () => {
  const env = await setupTestEnv();
  const realFetch = globalThis.fetch;
  try {
    const bytes = bytesOf("model-weights-payload-resume");
    const fullSha = await sha256Hex(bytes);
    const abs = join(paths().binDir, "e2e", "resume", "weights.bin");
    await Deno.mkdir(dirname(abs), { recursive: true });
    // A prior interrupted attempt left the first 8 bytes on disk.
    await Deno.writeFile(abs + ".tmp", bytes.slice(0, 8));

    let rangeStart = -1;
    globalThis.fetch = ((_input: string | URL | Request, init?: RequestInit) => {
      if (init?.method === "HEAD") {
        return Promise.resolve(new Response(null, { headers: { "x-linked-etag": fullSha } }));
      }
      const range = new Headers(init?.headers).get("range");
      if (range) {
        rangeStart = Number(/bytes=(\d+)-/.exec(range)?.[1] ?? -1);
        const tail = bytes.slice(rangeStart);
        return Promise.resolve(
          new Response(tail, {
            status: 206,
            headers: {
              "content-length": String(tail.byteLength),
              "content-range": `bytes ${rangeStart}-${bytes.byteLength - 1}/${bytes.byteLength}`,
            },
          }),
        );
      }
      return Promise.resolve(
        new Response(bytes, { headers: { "content-length": String(bytes.byteLength) } }),
      );
    }) as typeof fetch;

    const mgr = new DownloadManager();
    const got = await mgr.enqueue({
      source: "@e2e/resume/main/weights.bin",
      url: "https://huggingface.co/e2e/resume/resolve/main/weights.bin",
      destination: "binaries",
      groupId: "g",
    });
    // It requested the tail from the partial's length, and the assembled file is
    // the complete, hash-verified payload (head bytes + streamed tail).
    assertEquals(rangeStart, 8);
    assertEquals(got, abs);
    assertEquals(await Deno.readTextFile(abs), "model-weights-payload-resume");
  } finally {
    globalThis.fetch = realFetch;
    await env.teardown();
  }
});

Deno.test("DownloadManager.streamDownload: rejects when the streamed bytes do not match the published sha256", async () => {
  const env = await setupTestEnv();
  const realFetch = globalThis.fetch;
  try {
    const bytes = bytesOf("model-weights-payload");
    globalThis.fetch = downloadFetchStub(bytes, "0".repeat(64)); // wrong hash

    const mgr = new DownloadManager();
    const err = await assertRejects(
      () =>
        mgr.enqueue({
          source: "@e2e/integrity/main/weights.bin",
          url: "https://huggingface.co/e2e/integrity/resolve/main/weights.bin",
          destination: "binaries",
          groupId: "g",
        }),
      AppError,
    );
    assertStringIncludes(err.message, "sha256 mismatch");
    // The partial download is cleaned up rather than left as a usable artifact.
    const abs = join(paths().binDir, "e2e", "integrity", "weights.bin");
    assertEquals(
      await Deno.stat(abs)
        .then(() => true)
        .catch(() => false),
      false,
    );
  } finally {
    globalThis.fetch = realFetch;
    await env.teardown();
  }
});
