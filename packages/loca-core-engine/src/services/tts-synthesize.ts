// Text-to-Speech synthesis core, shared by the /tts/synthesize route and the
// module broker's tts.speak op. Local mode routes through the speech sidecar's
// Kokoro engine (queued behind the single engine for fair multi-client
// behaviour); external mode posts to the configured OpenAI-compatible endpoint
// (tts.external.{baseUrl, apiKey, model, voice}) via /v1/audio/speech. Mirrors
// stt-transcribe.ts.

import OpenAI from "openai";
import { host } from "../platform/runtime.ts";
import { loadCoreSettings } from "./core-settings.ts";
import { strSetting } from "./settings-access.ts";
import { AppError } from "../platform/errors.ts";
import { getLogger } from "../platform/log.ts";
import { classifyProviderError } from "./chat-provider-errors.ts";
import { EXTERNAL_TIMEOUT_MS, resolveExternalApiKey } from "./external-endpoint.ts";

const log = getLogger("tts");

/** Synthesize `text` to WAV bytes using the provider the settings select.
 *  `clientId` keys the local speech queue's per-client fairness (defaults to
 *  "core" for internal callers like the module broker). */
export async function synthesizeSpeech(
  text: string,
  voice?: string,
  speed?: number,
  clientId = "core",
): Promise<Uint8Array> {
  const settings = await loadCoreSettings();
  const provider = strSetting(settings, "tts.provider", "local");

  if (provider === "external") {
    const baseUrl = strSetting(settings, "tts.external.baseUrl", "");
    const model = strSetting(settings, "tts.external.model", "");
    if (!baseUrl || !model) {
      throw new AppError(
        "validation_error",
        "external TTS requires tts.external.baseUrl and tts.external.model",
      );
    }
    // Vault is authoritative; a plaintext value in settings.json is a
    // lower-precedence fallback. A keyless loopback gateway gets a placeholder;
    // any other host with no key is a misconfiguration we reject up front.
    const apiKey = await resolveExternalApiKey(settings, "tts.external.apiKey", baseUrl);
    if (!apiKey) {
      throw new AppError(
        "validation_error",
        "external TTS requires an API key (tts.external.apiKey)",
      );
    }
    // The `voice` argument is a local Kokoro voice id (the client always sends
    // tts.voice); it means nothing to an external provider, so the external
    // path uses the dedicated tts.external.voice setting instead.
    const selectedVoice = strSetting(settings, "tts.external.voice", "alloy");
    // Output stays WAV: the client playback queue and the module broker's
    // sample-rate probe both assume WAV bytes.
    const client = new OpenAI({
      baseURL: baseUrl,
      apiKey,
      maxRetries: 2,
      timeout: EXTERNAL_TIMEOUT_MS,
    });
    try {
      const startedAt = Date.now();
      log.info(`external synthesis starting (model ${model}, ${text.length} chars)`);
      const res = await client.audio.speech.create({
        model,
        voice: selectedVoice,
        input: text,
        response_format: "wav",
        ...(speed ? { speed } : {}),
      });
      const bytes = new Uint8Array(await res.arrayBuffer());
      log.info(
        `external synthesis done in ${Date.now() - startedAt}ms (out ${bytes.length} bytes)`,
      );
      return bytes;
    } catch (err) {
      const { code, message } = classifyProviderError(err);
      throw new AppError(code, `external TTS failed: ${message}`);
    }
  }

  // Local mode: hit the speech sidecar through the speech scheduler so
  // concurrent multi-client requests queue fairly behind the single engine.
  const local = host().localEndpoints;
  if (!local) throw new AppError("server_unavailable", "local TTS is not available on this host");
  return local.synthesize(text, voice, speed, clientId);
}
