import { assertEquals } from "@std/assert";
import type { BinaryKind, BinaryManifestEntry } from "@loca/shared";
import {
  __resetForTesting,
  binariesManager,
  onBinaryInstallFailed,
  resolveWithFallback,
} from "./manager.ts";
import { __resetResolverCacheForTesting } from "./upstream-resolver.ts";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { db } from "@loca/core-engine";
import { downloadManager } from "../downloads/manager.ts";

// Swap globalThis.fetch for a canned GitHub response so the resolver-entry cases
// run without touching the network (mirrors the helper in
// upstream-resolver.test.ts). A latest (un-pinned) resolver hits
// `/releases?per_page=N` (a newest-first ARRAY); pass a bare release object and
// it is wrapped as a one-element list.
function withMockRelease(body: unknown, fn: () => Promise<unknown>): () => Promise<void> {
  const list = Array.isArray(body) ? body : [body];
  return async () => {
    const orig = globalThis.fetch;
    __resetResolverCacheForTesting();
    globalThis.fetch = (input: string | URL | Request) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
      const payload = url.includes("/releases/tags/") ? list[0] : list;
      return Promise.resolve(
        new Response(JSON.stringify(payload), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      );
    };
    try {
      await fn();
    } finally {
      globalThis.fetch = orig;
      __resetResolverCacheForTesting();
    }
  };
}

Deno.test("resolveWithFallback: a pinned target resolves directly, no degrade, no network", async () => {
  const entry: BinaryManifestEntry = {
    version: "b1",
    platforms: {
      "x86_64-pc-windows-msvc": {
        cpu: { url: "https://e/cpu", sha256: "c" },
        cuda: { url: "https://e/cuda", sha256: "g" },
      },
    } as never,
  };
  const r = await resolveWithFallback(
    "llama-server",
    entry,
    "x86_64-pc-windows-msvc",
    "cuda",
    "cuda",
  );
  assertEquals(r?.variant, "cuda");
  assertEquals(r?.url, "https://e/cuda");
});

Deno.test(
  "resolveWithFallback: a missing upstream GPU asset degrades to the next offered variant (cuda -> vulkan)",
  withMockRelease(
    {
      tag_name: "b9999",
      assets: [
        {
          name: "llama-b9999-cpu.zip",
          browser_download_url: "https://gh/cpu.zip",
          digest: "sha256:c",
          size: 10,
        },
        {
          name: "llama-b9999-vulkan.zip",
          browser_download_url: "https://gh/vulkan.zip",
          digest: "sha256:v",
          size: 20,
        },
        // This release ships NO cuda asset (the maintenance-point case: llama.cpp
        // renamed/dropped it), so a cuda device must degrade rather than fail.
      ],
    },
    async () => {
      const entry: BinaryManifestEntry = {
        resolver: {
          repo: "ggml-org/llama.cpp",
          assets: {
            "x86_64-unknown-linux-gnu": {
              cpu: "llama-{tag}-cpu.zip",
              vulkan: "llama-{tag}-vulkan.zip",
              cuda: "llama-{tag}-cuda.zip",
            },
          },
        },
      };
      const r = await resolveWithFallback(
        "llama-server",
        entry,
        "x86_64-unknown-linux-gnu",
        "cuda",
        "cuda",
      );
      // Preference order for cuda is [cuda, directml, vulkan, cpu]; cuda's asset
      // is absent and directml isn't offered, so it lands on vulkan (not cpu).
      assertEquals(r?.variant, "vulkan");
      assertEquals(r?.url, "https://gh/vulkan.zip");
    },
  ),
);

Deno.test(
  "resolveWithFallback: the target resolves when its asset is present (no degrade)",
  withMockRelease(
    {
      tag_name: "b9999",
      assets: [
        {
          name: "llama-b9999-cpu.zip",
          browser_download_url: "https://gh/cpu.zip",
          digest: "sha256:c",
          size: 10,
        },
        {
          name: "llama-b9999-cuda.zip",
          browser_download_url: "https://gh/cuda.zip",
          digest: "sha256:g",
          size: 30,
        },
      ],
    },
    async () => {
      const entry: BinaryManifestEntry = {
        resolver: {
          repo: "ggml-org/llama.cpp",
          assets: {
            "x86_64-unknown-linux-gnu": {
              cpu: "llama-{tag}-cpu.zip",
              cuda: "llama-{tag}-cuda.zip",
            },
          },
        },
      };
      const r = await resolveWithFallback(
        "llama-server",
        entry,
        "x86_64-unknown-linux-gnu",
        "cuda",
        "cuda",
      );
      assertEquals(r?.variant, "cuda");
      assertEquals(r?.url, "https://gh/cuda.zip");
    },
  ),
);

Deno.test("install: a resolve failure records a per-kind failure and notifies listeners", async () => {
  // Drives the hub's requirements recompute: a binary that can't be resolved
  // must fire onBinaryInstallFailed (and expose failure()) instead of silently
  // dropping the kind. dev channel = in-code resolver manifest; the resolver's
  // GitHub call is rejected to force the failure.
  const prevChannel = Deno.env.get("LOCA_CHANNEL");
  Deno.env.set("LOCA_CHANNEL", "dev");
  const env = await setupTestEnv();
  const origFetch = globalThis.fetch;
  globalThis.fetch = () => Promise.reject(new Error("offline test"));
  __resetForTesting();
  const failed: BinaryKind[] = [];
  const off = onBinaryInstallFailed((k) => failed.push(k));
  try {
    await binariesManager().install(["llama-server"]);
    assertEquals(failed.includes("llama-server"), true);
    assertEquals(typeof binariesManager().failure("llama-server"), "string");
  } finally {
    off();
    globalThis.fetch = origFetch;
    __resetForTesting();
    await env.teardown();
    if (prevChannel === undefined) Deno.env.delete("LOCA_CHANNEL");
    else Deno.env.set("LOCA_CHANNEL", prevChannel);
  }
});

Deno.test("reconcileInterruptedInstalls: drops an orphaned binaries download row so it can't strand or land unextracted", async () => {
  const env = await setupTestEnv();
  const origFetch = globalThis.fetch;
  // Offline: loadBinaryManifest can't resolve, so reconcile removes the stale
  // row (its extraction context is gone with the old process) and leaves the
  // kind for retry rather than re-kicking. The removal is the invariant that
  // stops a resumed-but-never-installed archive AND the corebar limbo.
  globalThis.fetch = () => Promise.reject(new Error("offline test"));
  __resetForTesting();
  try {
    const id = "binaries:/fake/staging/llama-server-job1.tar.gz";
    db()
      .prepare(`
        INSERT INTO downloads
          (id, source, destination, rel_path, abs_path, filename, group_id,
           size_bytes, downloaded_bytes, status, error, added_at_ms)
        VALUES (?, 'https://cdn/llama.tar.gz', 'binaries', 'staging/llama-server-job1.tar.gz',
                '/fake/staging/llama-server-job1.tar.gz', 'llama-server-job1.tar.gz',
                'binary:llama-server', NULL, 0, 'Pending', NULL, ?)
      `)
      .run(id, Date.now());

    await binariesManager().reconcileInterruptedInstalls();

    // The orphaned row is gone, and no binaries download was re-armed (offline).
    const snap = downloadManager().snapshot();
    assertEquals(
      snap.some((r) => r.id === id),
      false,
    );
    assertEquals(
      snap.some((r) => r.destination === "binaries"),
      false,
    );
  } finally {
    globalThis.fetch = origFetch;
    __resetForTesting();
    await env.teardown();
  }
});
