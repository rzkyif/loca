// loca-core-speech sidecar: arg/readiness wiring plus the /configure, /speak,
// and /transcribe HTTP clients for the one resident process that serves both
// speech-to-text and text-to-speech across several sherpa-onnx model families.
//
// Which engines are loaded is the "desired state" derived from settings: local
// STT (stt.enabled + local provider + the model bundle on disk) and/or TTS
// (tts.enabled + the model bundle on disk). A config is tagged by its `family`
// and carries the resolved on-disk path for each sherpa role; the role names
// (encoder, decoder, joiner, model, voices, acoustic_model, vocoder, tokens, ...)
// match the binary's serde field names, so the bundle map serializes straight
// across. espeak-ng-data (the phonemizer for kokoro/kitten/vits/matcha) is NOT a
// model download: it ships inside the binary's archive at
// bin/lib/loca-core-speech/espeak-ng-data (see binaries/manager.ts) and is
// passed as the `data_dir` role by path.
//
// Two ways to drive the binary, both used by sidecar-boot's applySpeech and both
// carrying the SAME JSON config object:
//   - start flags (--stt-config / --tts-config <json>): the freshly-spawned
//     process loads those engines before it binds, so HTTP readiness == models
//     loaded.
//   - POST /configure {stt, tts}: the running process (re)loads or drops each
//     engine in place. Dropping one frees its model's memory while the other
//     stays resident, with no restart - preserving per-module unload-on-disable.

import { join } from "@std/path";
import {
  errMessage,
  getDefaultSettings,
  modelFilesMap,
  sttUsesLocal,
  ttsUsesLocal,
} from "@loca/shared";
import type { BinaryVariant } from "@loca/shared";
import { binPath, paths, speechPort } from "../paths.ts";
import { binaryName, libDirFor } from "../binaries/versions.ts";
import { installedVariant } from "../binaries/manager.ts";
import { resolveHfPath } from "../models/manager.ts";
import { numSetting, strSetting } from "@loca/core-engine/services/settings-access";
import { AppError } from "@loca/core-engine";
import type { StartOptions } from "./types.ts";

// Bound a single STT/TTS/configure call so a wedged inference (or a sidecar
// loading a multi-GB model) can't pin the caller forever.
const SPEECH_CALL_TIMEOUT_MS = 120_000;

/** A speech engine config: a `family` tag plus one resolved on-disk path per
 *  sherpa role. The role keys match the binary's serde field names; TTS configs
 *  also carry a `data_dir` role (the bundled espeak phonemizer dir, not a model
 *  download). */
export interface SpeechEngineConfig {
  family: string;
  [role: string]: string;
}
export type SpeechSttConfig = SpeechEngineConfig;
export type SpeechTtsConfig = SpeechEngineConfig;

/** The full desired engine state of the speech process. A null engine means
 *  "not loaded" (disabled, or its files aren't on disk yet). */
export interface SpeechState {
  stt: SpeechSttConfig | null;
  tts: SpeechTtsConfig | null;
  host: string;
  port: string;
  threads: number;
  /** ONNX Runtime execution provider passed to the binary (--provider), derived
   *  from the installed speech-binary variant. GPU providers only take effect on
   *  a GPU-built binary; on the CPU build sherpa/onnxruntime ignore it. */
  provider: string;
}

/** Map an installed speech-binary variant to the ONNX Runtime execution provider
 *  string the binary understands. Only the `cuda` GPU build exists (NVIDIA);
 *  every other variant (and none installed) runs on CPU. */
function providerForVariant(variant: BinaryVariant | null): string {
  return variant === "cuda" ? "cuda" : "cpu";
}

/** The bundled espeak-ng-data dir, extracted next to the binary at install. */
function espeakDataDir(): string {
  return join(libDirFor(paths().binDir, "loca-core-speech"), "espeak-ng-data");
}

/** Resolve the desired engine state from settings, gating each engine on its
 *  model files being present on disk: an enabled-but-not-yet-downloaded engine
 *  resolves to null so the process can still serve the other one, and the
 *  download-completion hook re-applies once the files land. The shared
 *  `stt.threads` knob drives the process thread count (STT is the heavier
 *  workload). Host/port are fixed loopback (the speech server is core-internal). */
export async function speechDesiredState(settings: Record<string, unknown>): Promise<SpeechState> {
  const threads = numSetting(settings, "stt.threads", 4);

  let stt: SpeechSttConfig | null = null;
  const sttLocal = sttUsesLocal(settings);
  if (sttLocal) {
    const family = strSetting(settings, "stt.modelType", schemaDefault("stt.modelType"));
    const roles = await resolveRoles(settings["stt.modelFiles"] ?? schemaDefault("stt.modelFiles"));
    if (family && roles) stt = { family, ...roles };
  }

  let tts: SpeechTtsConfig | null = null;
  const ttsLocal = ttsUsesLocal(settings);
  if (ttsLocal) {
    const family = strSetting(settings, "tts.modelType", schemaDefault("tts.modelType"));
    const roles = await resolveRoles(settings["tts.modelFiles"] ?? schemaDefault("tts.modelFiles"));
    // espeak phonemizer data ships with the binary, not as a download; pass it as
    // the data_dir role for the families that phonemize through espeak-ng.
    if (family && roles) tts = { family, ...roles, data_dir: espeakDataDir() };
  }

  const provider = providerForVariant(await installedVariant("loca-core-speech"));

  return { stt, tts, host: "127.0.0.1", port: String(speechPort()), threads, provider };
}

/** Resolve a `*.modelFiles` map (role -> HF spec) to role -> on-disk path,
 *  returning null if the value is malformed or any file is not yet downloaded
 *  (so the engine resolves to "not loaded" and the download-completion hook can
 *  re-apply once the files land). */
async function resolveRoles(modelFiles: unknown): Promise<Record<string, string> | null> {
  const map = modelFilesMap(modelFiles);
  if (!map) return null;
  const resolved: Record<string, string> = {};
  for (const [role, spec] of Object.entries(map)) {
    const path = resolveHfPath(spec);
    if (!(await fileExists(path))) return null;
    resolved[role] = path;
  }
  return resolved;
}

/** Build StartOptions whose `--stt-config`/`--tts-config <json>` flags load
 *  exactly the desired engines before the process binds, so HTTP readiness
 *  implies models loaded. The JSON is the same tagged config object POST
 *  /configure accepts. The CPU build statically links sherpa-onnx (no
 *  libraryDir); a GPU build (provider != cpu) links onnxruntime dynamically, so
 *  its GPU runtime + provider libs -- extracted into bin/lib/loca-core-speech --
 *  must be on the library path. */
export function buildSpeechStartOptions(state: SpeechState): StartOptions {
  const argv: string[] = [
    "--host",
    state.host,
    "--port",
    state.port,
    "--threads",
    String(state.threads),
    "--provider",
    state.provider,
  ];
  if (state.stt) argv.push("--stt-config", JSON.stringify(state.stt));
  if (state.tts) argv.push("--tts-config", JSON.stringify(state.tts));
  return {
    binary: binPath(binaryName("loca-core-speech")),
    args: argv,
    ...(state.provider !== "cpu"
      ? { libraryDir: libDirFor(paths().binDir, "loca-core-speech") }
      : {}),
    readiness: {
      kind: "http",
      url: `http://${state.host}:${state.port}/health`,
    },
    // Loading the turbo int8 model (~1 GB) on CPU can take a while.
    startupTimeoutMs: SPEECH_CALL_TIMEOUT_MS,
  };
}

function speechBase(): string {
  return `http://127.0.0.1:${speechPort()}`;
}

/** Reconcile a running process to `state` in place (no restart): each engine is
 *  (re)loaded when its paths change and dropped when null. Idempotent. */
export async function configureSpeech(state: SpeechState): Promise<void> {
  const body = JSON.stringify({ stt: state.stt, tts: state.tts });
  let res: Response;
  try {
    res = await fetch(`${speechBase()}/configure`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body,
      signal: AbortSignal.timeout(SPEECH_CALL_TIMEOUT_MS),
    });
  } catch (err) {
    throw new AppError("server_unavailable", `speech sidecar not reachable: ${errMessage(err)}`);
  }
  if (!res.ok) {
    throw new AppError(
      "provider_error",
      `speech /configure HTTP ${res.status}: ${await res.text().catch(() => "")}`,
    );
  }
}

/** Transcribe WAV bytes via the running sidecar. `signal` (the request abort)
 *  cancels the upstream inference if the client disconnects mid-decode. */
export async function speechTranscribe(audio: Blob, signal?: AbortSignal): Promise<string> {
  const timeout = AbortSignal.timeout(SPEECH_CALL_TIMEOUT_MS);
  const combined = signal ? AbortSignal.any([signal, timeout]) : timeout;
  let res: Response;
  try {
    res = await fetch(`${speechBase()}/transcribe`, {
      method: "POST",
      body: audio,
      signal: combined,
    });
  } catch (err) {
    throw new AppError("server_unavailable", `speech sidecar not reachable: ${errMessage(err)}`);
  }
  if (!res.ok) {
    throw new AppError("provider_error", `speech /transcribe HTTP ${res.status}`);
  }
  const json = (await res.json()) as { text?: string };
  return json.text ?? "";
}

/** Synthesize speech via the running sidecar; returns WAV bytes. `voice` is a
 *  tts.voice enum id (the binary maps it to a Kokoro speaker id via voices.rs). */
export async function speechSpeak(
  text: string,
  voice?: string,
  speed?: number,
): Promise<Uint8Array> {
  const payload: Record<string, unknown> = { text };
  if (voice) payload.voice = voice;
  if (typeof speed === "number" && Number.isFinite(speed)) {
    payload.speed = speed;
  }
  let res: Response;
  try {
    res = await fetch(`${speechBase()}/speak`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(payload),
      signal: AbortSignal.timeout(SPEECH_CALL_TIMEOUT_MS),
    });
  } catch (err) {
    throw new AppError("server_unavailable", `speech sidecar not reachable: ${errMessage(err)}`);
  }
  if (!res.ok) {
    throw new AppError(
      "provider_error",
      `speech /speak HTTP ${res.status}: ${await res.text().catch(() => "")}`,
    );
  }
  return new Uint8Array(await res.arrayBuffer());
}

async function fileExists(path: string): Promise<boolean> {
  try {
    return (await Deno.stat(path)).isFile;
  } catch {
    return false;
  }
}

function schemaDefault(key: string): string {
  const v = getDefaultSettings()[key];
  return typeof v === "string" ? v : "";
}
