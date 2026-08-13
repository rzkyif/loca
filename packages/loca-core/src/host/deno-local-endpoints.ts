// DenoHost local-inference endpoints: the loopback llama-server default port and
// the llama-embed sidecar endpoint. This is the one place the engine's LLM /
// embedding services reach the channel-adjusted local ports + models dir; a host
// without local inference (mobile) omits this provider and both LLM and
// embeddings are external-only. The sidecars boot on demand, but the embed model
// file must exist for embedEndpoint to return non-null.

import type { LocalEndpoints } from "@loca/core-engine";
import { EMBED_MODEL_FILE } from "@loca/shared";
import { embedPort, llmPort } from "../paths.ts";
import { resolveHfPath } from "../models/manager.ts";
import { speechSpeak, speechTranscribe } from "../sidecars/speech.ts";
import { speechScheduler } from "../services/speech-scheduler.ts";
import { sidecarManager } from "../sidecars/manager.ts";
import { loadModelCatalog } from "../models/catalog.ts";
import { selectedTtsVoices } from "../models/tts.ts";
import { denoFs } from "./deno-fs.ts";

export const denoLocalEndpoints: LocalEndpoints = {
  llmDefaultPort(): number {
    return llmPort();
  },
  async embedEndpoint(): Promise<{ url: string; model: string } | null> {
    const st = await denoFs.stat(resolveHfPath(EMBED_MODEL_FILE));
    if (st?.isDir !== false) return null; // not present (or a directory)
    return { url: `http://127.0.0.1:${embedPort()}/v1/embeddings`, model: "loca-embed" };
  },
  // Queue local speech behind the single engine so concurrent multi-client
  // requests are fair; the sidecar helpers bound + cancel the upstream call.
  transcribe(audio: File, clientId: string, signal?: AbortSignal): Promise<string> {
    return speechScheduler().schedule(clientId, () => speechTranscribe(audio, signal));
  },
  synthesize(
    text: string,
    voice: string | undefined,
    speed: number | undefined,
    clientId: string,
  ): Promise<Uint8Array> {
    return speechScheduler().schedule(clientId, () => speechSpeak(text, voice, speed));
  },
  speechStatus(): { running: boolean; loading: boolean } {
    const s = sidecarManager().status("speech").status;
    return { running: s === "Running", loading: s === "Loading" };
  },
  async ttsVoices(
    settings: Record<string, unknown>,
  ): Promise<Array<{ id: string; label: string; lang?: string }>> {
    const catalog = await loadModelCatalog();
    return selectedTtsVoices(
      catalog,
      String(settings["tts.modelType"] ?? "kokoro"),
      String(settings["tts.modelPath"] ?? ""),
    );
  },
};
