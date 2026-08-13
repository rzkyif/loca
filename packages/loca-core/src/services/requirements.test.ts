// computeRequirements classification: present vs missing across models +
// binaries. Network is stubbed (reject) so binary manifest/probe calls degrade
// gracefully and the test stays offline + deterministic; classification comes
// from on-disk presence (model files / binaries), not the network.

import { assertEquals } from "@std/assert";
import { dirname } from "@std/path";
import { binarySource, EMBED_MODEL_FILE } from "@loca/shared";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { patchCoreSettings } from "@loca/core-engine/services/core-settings";
import { computeRequirements } from "./requirements.ts";
import { resolveHfPath } from "../models/manager.ts";
import { __resetForTesting, binariesManager } from "../binaries/manager.ts";
import { __resetForTesting as __resetDownloads, downloadManager } from "../downloads/manager.ts";
import { sourcesForKind } from "./model-ensure.ts";

async function writeStub(absPath: string): Promise<void> {
  await Deno.mkdir(dirname(absPath), { recursive: true });
  await Deno.writeTextFile(absPath, "stub");
}

Deno.test("computeRequirements: classifies present models + missing binaries; gates by settings", async () => {
  const env = await setupTestEnv();
  const origFetch = globalThis.fetch;
  globalThis.fetch = () => Promise.reject(new Error("offline test"));
  try {
    // External LLM + STT/TTS disabled: required models = the embed GGUF only;
    // required binaries = deno + llama-server (embeddings; no whisper-server).
    await patchCoreSettings({
      "llm.provider": "external",
      "stt.enabled": false,
      "tts.enabled": false,
    });
    // Pre-create the embed model so probe short-circuits (present, no fetch).
    await writeStub(resolveHfPath(EMBED_MODEL_FILE));

    const snap = await computeRequirements();

    // Embed model present.
    const embed = snap.required.filter((r) => r.type === "model" && r.group === "embed");
    assertEquals(embed.length, 1);
    assertEquals(
      embed.every((r) => r.present),
      true,
    );

    // Binary set is settings-derived: deno + llama-server required, whisper not.
    const binSources = snap.required
      .filter((r) => r.type === "binary")
      .map((r) => r.source)
      .sort();
    assertEquals(binSources, [binarySource("deno"), binarySource("llama-server")]);

    // deno + llama-server not on disk -> missing; embed present -> not missing.
    assertEquals(
      snap.missing.map((m) => m.source),
      [binarySource("deno"), binarySource("llama-server")],
    );
  } finally {
    globalThis.fetch = origFetch;
    await env.teardown();
  }
});

Deno.test("computeRequirements: a failed model download surfaces its error, still missing", async () => {
  // Model parity with the binary case: a model download that errored (here, the
  // transfer's fetch is rejected) lives on as an Error row in the queue; the
  // required-file entry must carry that error so it's retryable, not a silent
  // perpetually-"downloading" gate.
  const env = await setupTestEnv();
  __resetDownloads();
  const origFetch = globalThis.fetch;
  try {
    await patchCoreSettings({
      "llm.provider": "external",
      "stt.enabled": false,
      "tts.enabled": false,
    });
    // Embeddings are always required; grab the embed source and fail its download.
    const [embedSource] = await sourcesForKind("embed");
    globalThis.fetch = () => Promise.reject(new Error("offline test"));
    await downloadManager()
      .enqueue({ source: embedSource, destination: "models", groupId: "embed" })
      .catch(() => {}); // expected: the transfer fetch rejects -> Error row

    const snap = await computeRequirements();
    const embed = snap.required.find((r) => r.type === "model" && r.source === embedSource);
    assertEquals(embed?.present, false);
    assertEquals(typeof embed?.error, "string");
    // And it's still in the gating `missing` set.
    assertEquals(
      snap.missing.some((m) => m.source === embedSource),
      true,
    );
  } finally {
    globalThis.fetch = origFetch;
    __resetDownloads();
    await env.teardown();
  }
});

Deno.test("computeRequirements: a failed binary install surfaces a retryable error, still missing", async () => {
  // The reported limbo: a required binary that can't be resolved/installed must
  // NOT vanish silently. It stays in `missing` (still gates the app) but now
  // carries `error`, so the UI shows a retry instead of a sizeless dead-end.
  // dev channel = in-code resolver manifest (no signed fetch); the resolver's
  // GitHub call still hits the network, which we reject to force a failure.
  const prevChannel = Deno.env.get("LOCA_CHANNEL");
  Deno.env.set("LOCA_CHANNEL", "dev");
  const env = await setupTestEnv();
  const origFetch = globalThis.fetch;
  globalThis.fetch = () => Promise.reject(new Error("offline test"));
  __resetForTesting(); // fresh binaries manager (clean failures map)
  try {
    await patchCoreSettings({
      "llm.provider": "external",
      "stt.enabled": false,
      "tts.enabled": false,
    });
    // Attempt the install: kickoff's resolve fails (fetch rejects), so the
    // manager records a per-kind failure without throwing to us.
    await binariesManager().install(["llama-server"]);

    const snap = await computeRequirements();
    const llama = snap.missing.find((m) => m.source === binarySource("llama-server"));
    assertEquals(!!llama, true); // still blocks
    assertEquals(typeof llama?.error, "string"); // ...but retryable, not silent
  } finally {
    globalThis.fetch = origFetch;
    __resetForTesting();
    await env.teardown();
    if (prevChannel === undefined) Deno.env.delete("LOCA_CHANNEL");
    else Deno.env.set("LOCA_CHANNEL", prevChannel);
  }
});
