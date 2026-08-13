import { Hono, type MiddlewareHandler } from "hono";
import { host } from "../../platform/runtime.ts";
import { loadCoreSettings } from "../../services/core-settings.ts";
import { synthesizeSpeech } from "../../services/tts-synthesize.ts";
import { TTS_MAX_TEXT_CHARS } from "../../services/media-limits.ts";
import { AppError } from "../../platform/errors.ts";
import { readJson } from "../body.ts";
import { requireClient } from "../middleware/auth.ts";

export function ttsRoutes(authed: MiddlewareHandler): Hono {
  const r = new Hono();
  r.use("*", authed);

  // TTS loads and unloads with the speech sidecar (gated on tts.enabled in
  // services/sidecar-boot.ts), so explicit prewarm / unload are no-ops kept
  // for the client's existing calls.
  r.post("/load", (c) => c.body(null, 204));
  r.post("/unload", (c) => c.body(null, 204));

  r.post("/synthesize", async (c) => {
    const body = (await readJson(c)) as {
      text?: string;
      voice?: string;
      speed?: number;
    };
    if (!body.text || typeof body.text !== "string") {
      throw new AppError("validation_error", "text required");
    }
    if (body.text.length > TTS_MAX_TEXT_CHARS) {
      throw new AppError("validation_error", `tts text exceeds ${TTS_MAX_TEXT_CHARS} characters`);
    }
    const wav = await synthesizeSpeech(body.text, body.voice, body.speed, requireClient(c).id);
    // Copy into an ArrayBuffer-backed view so the Response body type is concrete
    // (synthesize returns Uint8Array<ArrayBufferLike>).
    return new Response(new Uint8Array(wav), {
      status: 200,
      headers: { "Content-Type": "audio/wav" },
    });
  });

  // The voices of the currently-selected local TTS model, so the client's voice
  // dropdown matches the model in use. A host without local inference has no
  // local catalog, so it returns none.
  r.get("/voices", async (c) => {
    const settings = await loadCoreSettings();
    const voices = (await host().localEndpoints?.ttsVoices(settings)) ?? [];
    return c.json(voices);
  });

  // Map the speech sidecar's lifecycle onto the {loaded, loading} shape the
  // client polls: TTS is "loaded" once the sidecar is Running with tts enabled.
  r.get("/status", async (c) => {
    const st = host().localEndpoints?.speechStatus() ?? { running: false, loading: false };
    const settings = await loadCoreSettings();
    const ttsOn = settings["tts.enabled"] === true;
    return c.json({
      loaded: ttsOn && st.running,
      loading: ttsOn && st.loading,
    });
  });

  return r;
}
