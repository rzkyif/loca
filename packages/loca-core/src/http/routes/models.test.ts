// HTTP contract for /api/v1/models. Network-heavy actions
// (download, probe of remote sources) are exercised indirectly via the
// fast paths (alreadyHave / validation rejections); the network surface
// is covered separately by manager + sources tests.

import { assert, assertEquals } from "@std/assert";
import { dirname } from "@std/path";
import type { RequirementsSnapshot } from "@loca/shared";
import { EMBED_MODEL_FILE, settingKeyDestination } from "@loca/shared";
import { buildApp } from "../server.ts";
import { pairClient } from "../../../tests/helpers/pairing.ts";
import { setupTestEnv } from "../../../tests/helpers/db.ts";
import { patchCoreSettings } from "@loca/core-engine/services/core-settings";
import { onRequirementsChanged } from "../../services/requirements.ts";
import { resolveHfPath } from "../../models/manager.ts";

async function pairOne(): Promise<{ token: string }> {
  const { token } = await pairClient("t2", "127.0.0.1");
  return { token };
}

function bearer(token: string): Record<string, string> {
  return { authorization: `Bearer ${token}` };
}

Deno.test("GET /api/v1/models: requires bearer (401)", async () => {
  const env = await setupTestEnv();
  try {
    const app = buildApp();
    const res = await app.fetch(new Request("http://x/api/v1/models"));
    assertEquals(res.status, 401);
  } finally {
    await env.teardown();
  }
});

Deno.test("GET /api/v1/models/stt/catalog: requires bearer (401)", async () => {
  const env = await setupTestEnv();
  try {
    const app = buildApp();
    const res = await app.fetch(new Request("http://x/api/v1/models/stt/catalog"));
    assertEquals(res.status, 401);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/models/stt/select: requires bearer (401)", async () => {
  const env = await setupTestEnv();
  try {
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/models/stt/select", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ presetId: "accurate" }),
      }),
    );
    assertEquals(res.status, 401);
  } finally {
    await env.teardown();
  }
});

Deno.test("GET /api/v1/models: returns empty array on a fresh tempdir", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(new Request("http://x/api/v1/models", { headers: bearer(token) }));
    assertEquals(res.status, 200);
    assertEquals(await res.json(), []);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/models/ensure: bad kind returns 400 with validation_error", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/models/ensure", {
        method: "POST",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ kind: "lol" }),
      }),
    );
    assertEquals(res.status, 400);
    const body = await res.json();
    assertEquals(body.error.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/models/probe: malformed sources array returns 400", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/models/probe", {
        method: "POST",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ sources: "not-an-array" }),
      }),
    );
    assertEquals(res.status, 400);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/models/download: missing items array returns 400", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/models/download", {
        method: "POST",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({}),
      }),
    );
    assertEquals(res.status, 400);
  } finally {
    await env.teardown();
  }
});

Deno.test("DELETE /api/v1/models/:relPath recomputes requirements so the file flips to missing", async () => {
  const env = await setupTestEnv();
  const origFetch = globalThis.fetch;
  // Offline: missing-file probes degrade gracefully instead of hitting HF.
  globalThis.fetch = () => Promise.reject(new Error("offline test"));
  let resolveSnap: (s: RequirementsSnapshot) => void = () => {};
  const recomputed = new Promise<RequirementsSnapshot>((r) => (resolveSnap = r));
  const unsub = onRequirementsChanged((snap) => resolveSnap(snap));
  try {
    const { token } = await pairOne();
    // Minimize required models to the embed GGUF (external LLM, STT/TTS off),
    // and put it on disk so it reads as present first.
    await patchCoreSettings({
      "llm.provider": "external",
      "stt.enabled": false,
      "tts.enabled": false,
    });
    const target = EMBED_MODEL_FILE;
    const abs = resolveHfPath(target);
    await Deno.mkdir(dirname(abs), { recursive: true });
    await Deno.writeTextFile(abs, "stub");

    const app = buildApp();
    // relPath under the models root (HF branch dropped on disk).
    const res = await app.fetch(
      new Request(
        "http://x/api/v1/models/second-state/All-MiniLM-L6-v2-Embedding-GGUF/all-MiniLM-L6-v2-Q8_0.gguf",
        {
          method: "DELETE",
          headers: bearer(token),
        },
      ),
    );
    assertEquals(res.status, 204);

    // The route fires notifyRequirementsChanged (fire-and-forget); the listener
    // resolves with the fresh snapshot, which must now flag the deleted file.
    const snap = await recomputed;
    assert(snap.missing.some((m) => m.source === target));
  } finally {
    unsub();
    globalThis.fetch = origFetch;
    await env.teardown();
  }
});

// POST /select applies a model by writing the keys in models.ts `applyToPatch`,
// then partitions them by destination: shared model/server keys go to the core
// store, the model's recommended sampling/reasoning tuning to the caller's
// per-client overlay. This locks the destination contract that partition relies
// on, so sampling can never regress back into the shared store (where it would
// broadcast to and overwrite every other client's baseline). The catalog fetch
// makes a full /select round-trip network-bound, so the route path itself is
// covered manually; the partition mechanism is the same one settings.test.ts
// exercises end to end.
Deno.test("model /select: applied keys partition to the expected destinations", () => {
  const sharedModelServerKeys = [
    "llm.provider",
    "llm.preset",
    "llm.modelPath",
    "llm.mmprojPath",
    "llm.contextSize",
    "llm.threads",
    "llm.gpuLayers",
    "llm.flashAttn",
    // llm.supportImages is intentionally not in the patch (user-owned, decoupled
    // from the preset), so it isn't part of this partition contract.
    "llm.idleUnloadSeconds",
  ];
  const perClientTuningKeys = [
    "llm.reasoningBudget",
    "llm.temperature",
    "llm.topP",
    "llm.topK",
    "llm.minP",
    "llm.repeatPenalty",
  ];
  for (const k of sharedModelServerKeys) assertEquals(settingKeyDestination(k), "core");
  for (const k of perClientTuningKeys) assertEquals(settingKeyDestination(k), "client-on-core");
});

Deno.test("GET /api/v1/models/downloads: empty list before any enqueue", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/models/downloads", {
        headers: bearer(token),
      }),
    );
    assertEquals(res.status, 200);
    assertEquals(await res.json(), []);
  } finally {
    await env.teardown();
  }
});
