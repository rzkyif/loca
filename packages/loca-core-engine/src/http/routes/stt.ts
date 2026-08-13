// STT routes. Transcription provider branching (local speech sidecar vs
// external OpenAI-compatible endpoint) lives in services/stt-transcribe.ts,
// shared with the module broker's stt.transcribe op.
//
// Whether the local speech sidecar is running is a host concern
// (host.localEndpoints.speechStatus); a host without local inference omits it,
// so local status reads as "not running" there.

import { Hono, type MiddlewareHandler } from "hono";
import { host } from "../../platform/runtime.ts";
import { loadCoreSettings } from "../../services/core-settings.ts";
import { transcribeAudio } from "../../services/stt-transcribe.ts";
import { STT_MAX_AUDIO_BYTES } from "../../services/media-limits.ts";
import { AppError } from "../../platform/errors.ts";
import { requireClient } from "../middleware/auth.ts";

export function sttRoutes(authed: MiddlewareHandler): Hono {
  const r = new Hono();
  r.use("*", authed);

  r.post("/transcribe", async (c) => {
    const form = await c.req.formData();
    const audio = form.get("audio");
    if (!(audio instanceof File)) {
      throw new AppError("validation_error", "audio file required");
    }
    if (audio.size > STT_MAX_AUDIO_BYTES) {
      throw new AppError("validation_error", "stt audio exceeds the 25 MB limit");
    }
    const language = form.get("language");
    const languageStr = typeof language === "string" && language.length > 0 ? language : undefined;
    const settings = await loadCoreSettings();
    // The request-abort signal cancels the upstream inference if the client
    // disconnects mid-decode.
    const text = await transcribeAudio(
      settings,
      audio,
      languageStr,
      c.req.raw.signal,
      requireClient(c).id,
    );
    return c.json({ text });
  });

  r.get("/status", async (c) => {
    const settings = await loadCoreSettings();
    const provider = settings["stt.provider"] === "external" ? "external" : "local";
    // External transcription has no local sidecar to poll; it is "running"
    // whenever it is the selected provider. Local follows the speech sidecar.
    const running =
      provider === "external" ? true : (host().localEndpoints?.speechStatus().running ?? false);
    return c.json({ provider, running });
  });

  return r;
}
