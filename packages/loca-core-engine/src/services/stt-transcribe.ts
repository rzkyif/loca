// Speech-to-Text transcription core, shared by the /stt/transcribe route
// and the module broker's stt.transcribe op. Local mode posts the audio to
// the speech sidecar (loca-core-speech /transcribe); external mode posts to
// the configured OpenAI-compatible endpoint (stt.external.{baseUrl, apiKey,
// model}).

import OpenAI from "openai";
import { host } from "../platform/runtime.ts";
import { AppError } from "../platform/errors.ts";
import { getLogger } from "../platform/log.ts";
import { strSetting } from "./settings-access.ts";
import { classifyProviderError } from "./chat-provider-errors.ts";
import { EXTERNAL_TIMEOUT_MS, resolveExternalApiKey } from "./external-endpoint.ts";

const log = getLogger("stt");

/** Transcribe `audio` using the provider the settings select. `signal`
 *  bounds the local-sidecar call on top of the built-in timeout (callers
 *  pass their request-abort signal; omit for the broker path). `clientId`
 *  keys the local speech queue's per-client fairness (defaults to "core" for
 *  internal callers like the module broker). */
export async function transcribeAudio(
  settings: Record<string, unknown>,
  audio: File,
  language: string | undefined,
  signal?: AbortSignal,
  clientId = "core",
): Promise<string> {
  const provider = strSetting(settings, "stt.provider", "local");

  if (provider === "external") {
    const baseUrl = strSetting(settings, "stt.external.baseUrl", "");
    const model = strSetting(settings, "stt.external.model", "");
    if (!baseUrl || !model) {
      throw new AppError(
        "validation_error",
        "external STT requires stt.external.baseUrl and stt.external.model",
      );
    }
    // Vault is authoritative; a plaintext value in settings.json is a
    // lower-precedence fallback. A keyless loopback gateway gets a placeholder;
    // any other host with no key is a misconfiguration we reject up front.
    const apiKey = await resolveExternalApiKey(settings, "stt.external.apiKey", baseUrl);
    if (!apiKey) {
      throw new AppError(
        "validation_error",
        "external STT requires an API key (stt.external.apiKey)",
      );
    }
    // A request-pinned language hint (per call) overrides the configured
    // default; local auto-detects, so the hint applies to the external path.
    const lang = language || strSetting(settings, "stt.external.language", "") || undefined;
    // Bound the call at 120s to match the local sidecar. Never pass
    // `timeout: 0`: the SDK treats it as a literal 0ms deadline and aborts
    // every request instantly with "Request timed out".
    const client = new OpenAI({
      baseURL: baseUrl,
      apiKey,
      maxRetries: 2,
      timeout: EXTERNAL_TIMEOUT_MS,
    });
    try {
      const startedAt = Date.now();
      log.info(`external transcription starting (audio ${audio.size} bytes, model ${model})`);
      const res = await client.audio.transcriptions.create(
        {
          model,
          file: audio,
          ...(lang ? { language: lang } : {}),
          response_format: "json",
        },
        // Cancel the upstream request if the caller aborts (client disconnect),
        // matching the local path so an abandoned transcription stops billing.
        { signal },
      );
      const text = (res as { text?: string }).text ?? "";
      log.info(
        `external transcription done in ${Date.now() - startedAt}ms ` +
          `(audio ${audio.size} bytes, out ${text.length} chars)`,
      );
      return text;
    } catch (err) {
      const { code, message } = classifyProviderError(err);
      throw new AppError(code, `external STT failed: ${message}`);
    }
  }

  // Local mode: hit the speech sidecar through the speech scheduler so
  // concurrent multi-client requests queue fairly behind the single engine.
  // speechTranscribe bounds the upstream call and cancels it when the caller
  // aborts, so neither a wedged sidecar nor an abandoned request keeps the
  // Whisper inference running indefinitely. The resident model auto-detects the
  // spoken language, so the `language` hint applies to the external provider
  // only.
  const startedAt = Date.now();
  log.info(`local transcription starting (audio ${audio.size} bytes)`);
  const local = host().localEndpoints;
  if (!local) throw new AppError("server_unavailable", "local STT is not available on this host");
  const text = await local.transcribe(audio, clientId, signal);
  log.info(
    `local transcription done in ${Date.now() - startedAt}ms ` +
      `(audio ${audio.size} bytes, out ${text.length} chars)`,
  );
  return text;
}
