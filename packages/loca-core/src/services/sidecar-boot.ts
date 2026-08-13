// Sidecar boot orchestrator. Starts the llama, llama-embed, and speech sidecars
// based on the current settings, then re-evaluates on every settings PATCH and
// (re)starts / stops / reconfigures the affected sidecar.
//
// This is the only place that decides whether a sidecar should be running.
// `sidecarManager()` is a generic supervisor; the kind-specific gates
// (provider, enabled toggle, model file existence) live here.

import {
  buildLlamaStartOptionsScaled,
  llamaMissingPrereq,
  llamaStartArgsFromSettings,
} from "../sidecars/llama.ts";
import {
  buildLlamaEmbedStartOptions,
  llamaEmbedStartArgsFromSettings,
} from "../sidecars/llama-embed.ts";
import { errMessage } from "@loca/shared";
import {
  buildSpeechStartOptions,
  configureSpeech,
  speechDesiredState,
  type SpeechState,
} from "../sidecars/speech.ts";
import { sidecarManager } from "../sidecars/manager.ts";
import { llmScheduler } from "@loca/core-engine/services/llm-scheduler";
import { llmIdle } from "./llm-idle.ts";
import { loadCoreSettings, subscribeCoreSettings } from "@loca/core-engine/services/core-settings";
import { strSetting } from "@loca/core-engine/services/settings-access";
import { downloadManager } from "../downloads/manager.ts";
import { onBinaryInstalled } from "../binaries/manager.ts";
import { runQueuedBuiltinInstall } from "../extensions/seeding.ts";
import { BROADCAST_SINK } from "../http/routes/extensions.ts";
import { binaryName } from "../binaries/versions.ts";
import { binPath } from "../paths.ts";
import { getLogger } from "../shared/log.ts";

const log = getLogger("sidecar-boot");

// Keys that, when changed, should trigger a llama-server restart with the
// new args (or a stop if the gating condition flipped off). Thinking and the
// sampling knobs (llm.reasoning, llm.reasoningBudget, llm.temperature, etc.) are
// deliberately absent: they are sent per request, so editing them takes effect
// on the next turn without a restart.
const LLAMA_KEYS = new Set([
  "llm.provider",
  "llm.modelPath",
  "llm.mmprojPath",
  "llm.supportImages",
  "llm.host",
  "llm.port",
  "llm.threads",
  "llm.contextSize",
  "llm.mmap",
  "llm.webui",
  "llm.gpuLayers",
  "llm.flashAttn",
]);

// Keys that, when changed, reconcile the speech sidecar. STT and TTS share one
// process: stt.threads drives the process thread count and the rest gate which
// engines load, so tts.enabled lives here too. The TTS model-selection keys are
// included so switching preset/model (which writes tts.modelType/modelFiles, not
// tts.enabled) reloads the engine in place instead of leaving the old voice
// resident until the user manually toggles TTS off and on.
const SPEECH_KEYS = new Set([
  "stt.enabled",
  "stt.provider",
  "stt.modelPath",
  "stt.threads",
  "tts.enabled",
  "tts.modelType",
  "tts.modelPath",
  "tts.modelFiles",
]);

// The embed sidecar only reads the shared CPU-thread knob; its model is fixed.
// `llm.provider` + `llm.external.embedModel` gate whether embeddings run on the
// local sidecar or an external provider, so a change to either re-applies (and
// may stop) the embed sidecar.
const EMBED_KEYS = new Set(["llm.threads", "llm.provider", "llm.external.embedModel"]);

let initialized = false;

// Tracks the last-seen status per download id so the subscriber can fire
// only on the "just transitioned to Completed" edge rather than every
// progress tick.
const lastSeenStatus = new Map<string, string>();

/** Called once from main.ts at startup. Boots the relevant sidecars from
 *  the persisted settings, then installs a listener for live restarts and
 *  a download-completion hook for auto-restart after a model finishes
 *  downloading. */
export async function initSidecarBoot(): Promise<void> {
  if (initialized) return;
  initialized = true;
  // Wire the LLM scheduler watchdog so a wedged llama-server actually self-heals:
  // when a local slot stays held past callTimeoutMs + grace, the scheduler aborts
  // the upstream stream and calls this handler, which restarts the sidecar so
  // subsequent requests hit a healthy server. Wiring this handler is what makes
  // the self-healing actually fire.
  llmScheduler().setWatchdogHandler(({ clientId, elapsedMs }) => {
    log.error(
      `llm watchdog: client ${clientId} slot wedged ${elapsedMs}ms; restarting llama-server`,
    );
    void (async () => {
      const s = await loadCoreSettings();
      await applyLlama(s).catch(logErr("llama"));
    })();
  });
  const settings = await loadCoreSettings();
  llmIdle().configure(settings);
  await applyLlama(settings).catch(logErr("llama"));
  await applyLlamaEmbed(settings).catch(logErr("llama-embed"));
  await applySpeech(settings).catch(logErr("speech"));
  subscribeCoreSettings((next, changed) => {
    if (changed.has("llm.idleUnloadSeconds")) llmIdle().configure(next);
    // Fire-and-forget the restarts: a sidecar relaunch loads a multi-GB model
    // and takes seconds, so awaiting it here would stall every settings PATCH
    // (e.g. the response to /models/select). The sidecar reports its loading
    // state to the client separately. Matches the download/binary hooks below.
    if (anyOverlap(changed, LLAMA_KEYS)) {
      void applyLlama(next).catch(logErr("llama"));
    }
    if (anyOverlap(changed, EMBED_KEYS)) {
      void applyLlamaEmbed(next).catch(logErr("llama-embed"));
    }
    if (anyOverlap(changed, SPEECH_KEYS)) {
      void applySpeech(next).catch(logErr("speech"));
    }
  });
  // When an llm/stt model file finishes downloading, retry the affected
  // sidecar. Without this hook the user has to manually toggle settings
  // (or restart core) after the download completes to actually pick up
  // the new file.
  // Seed with the persisted rows first: download rows survive restarts, so on
  // the first broadcast after boot a Completed row from an earlier run would
  // otherwise read as a fresh completion and spuriously restart the sidecars
  // (a multi-GB model reload) the moment anything touches the download queue.
  for (const entry of downloadManager().snapshot()) {
    lastSeenStatus.set(entry.id, entry.status);
  }
  downloadManager().subscribe((snap) => {
    const justCompleted = new Set<string>();
    for (const entry of snap) {
      const prev = lastSeenStatus.get(entry.id);
      if (prev !== "Completed" && entry.status === "Completed") {
        justCompleted.add(entry.groupId);
      }
      lastSeenStatus.set(entry.id, entry.status);
    }
    if (justCompleted.size === 0) return;
    void (async () => {
      const s = await loadCoreSettings();
      if (justCompleted.has("llm")) {
        await applyLlama(s).catch(logErr("llama"));
      }
      if (justCompleted.has("embed")) {
        await applyLlamaEmbed(s).catch(logErr("llama-embed"));
      }
      if (justCompleted.has("stt") || justCompleted.has("tts")) {
        await applySpeech(s).catch(logErr("speech"));
      }
    })();
  });
  // Binary installs don't surface through the download-completion hook above:
  // their model `groupId` is `binary:<kind>` and the .tar.gz download reports
  // Completed *before* extraction finishes, so the on-disk executable isn't
  // there yet at that point. `onBinaryInstalled` fires after extraction, so a
  // sidecar whose binary was missing (and was therefore left Disabled) can
  // start once its binary actually lands.
  onBinaryInstalled((kind) => {
    void (async () => {
      const s = await loadCoreSettings();
      if (kind === "llama-server") {
        await applyLlama(s).catch(logErr("llama"));
        await applyLlamaEmbed(s).catch(logErr("llama-embed"));
      }
      if (kind === "loca-core-speech") {
        await applySpeech(s).catch(logErr("speech"));
      }
      if (kind === "deno") {
        // The deno worker runtime just landed: if the user asked to install the
        // built-in's tools while it was still missing, run that queued install now.
        await runQueuedBuiltinInstall(BROADCAST_SINK).catch(logErr("builtin-queued-install"));
      }
    })();
  });
}

// (idle-unload supervisor; configured above + on settings change)

export async function applyLlama(settings: Record<string, unknown>): Promise<void> {
  const args = llamaStartArgsFromSettings(settings);
  if (!args) {
    log.info(
      "llama-server gated off by settings (external provider or no model path); not running",
    );
    await sidecarManager().stop("llama");
    return;
  }
  const missing = await llamaMissingPrereq(args);
  if (missing) {
    log.warn(
      `llama-server ${missing}; sidecar stays Disabled until the user ` +
        `downloads it (requirements flow). The download-completion / ` +
        `binary-install hooks re-apply once the file lands`,
    );
    await sidecarManager().stop("llama");
    return;
  }
  // Scale the readiness window to the model size so a large model on slow
  // hardware isn't false-timed-out into the flap guard.
  await sidecarManager().restart("llama", await buildLlamaStartOptionsScaled(args));
}

// Embeddings are always-on infrastructure (no enable toggle): run llama-embed
// whenever its model + the llama-server binary are on disk. Mirrors applyLlama's
// disk gates (but no provider/mmproj gate) so it stays Disabled until the MiniLM
// GGUF + binary land, then the download/binary hooks re-apply.
export async function applyLlamaEmbed(settings: Record<string, unknown>): Promise<void> {
  // When embeddings are served by an external provider (llm in external mode
  // with a Relevance Model set), the local embed sidecar isn't needed.
  const embedModel = strSetting(settings, "llm.external.embedModel", "").trim();
  if (strSetting(settings, "llm.provider", "local") === "external" && embedModel) {
    await sidecarManager().stop("llama-embed");
    return;
  }
  const args = llamaEmbedStartArgsFromSettings(settings);
  if (!(await fileExists(args.modelPath))) {
    log.warn(
      `llama-embed model not on disk: ${args.modelPath}; sidecar stays ` +
        `Disabled until the user downloads it (requirements flow). The ` +
        `download-completion hook re-applies once the file lands`,
    );
    await sidecarManager().stop("llama-embed");
    return;
  }
  if (!(await fileExists(binPath(binaryName("llama-server"))))) {
    log.warn(
      `llama-embed binary (llama-server) not installed; sidecar stays Disabled ` +
        `until it is downloaded`,
    );
    await sidecarManager().stop("llama-embed");
    return;
  }
  await sidecarManager().restart("llama-embed", buildLlamaEmbedStartOptions(args));
}

// Tracks the process identity (host/port/threads) the speech sidecar was last
// (re)started with. Changing which engines are loaded, or the STT model, is
// applied via POST /configure on the RUNNING process (no restart) so dropping
// one engine frees only its model's memory; a restart is forced only when the
// identity changes (thread count) or the process isn't up yet.
let speechProcKey: string | null = null;

function speechProcKeyOf(state: SpeechState): string {
  // Include provider: it is fixed at process start (the ONNX EP is chosen when
  // an engine is built), so a provider change (new GPU binary variant landed)
  // must force a restart rather than a live /configure.
  return `${state.host}:${state.port}:${state.threads}:${state.provider}`;
}

// The combined speech sidecar (Whisper STT + Kokoro TTS). Runs whenever at
// least one engine is wanted AND its model is on disk; the binary must be
// installed first. Mirrors applyLlamaEmbed's disk gating, but reconfigures the
// running process in place when only the engine selection changed.
export async function applySpeech(settings: Record<string, unknown>): Promise<void> {
  const state = await speechDesiredState(settings);
  if (!state.stt && !state.tts) {
    log.info(
      "speech sidecar gated off (STT disabled/external and TTS disabled, or models not on disk); not running",
    );
    await sidecarManager().stop("speech");
    speechProcKey = null;
    return;
  }
  if (!(await fileExists(binPath(binaryName("loca-core-speech"))))) {
    log.warn(
      `loca-core-speech binary not installed; sidecar stays Disabled until it ` +
        `is downloaded (the client re-prompts the pending download on the ` +
        `next settings change)`,
    );
    await sidecarManager().stop("speech");
    speechProcKey = null;
    return;
  }
  const procKey = speechProcKeyOf(state);
  if (sidecarManager().status("speech").status === "Running" && procKey === speechProcKey) {
    // Process already up with the right host/port/threads: reconfigure engines
    // in place so a now-disabled module frees only its own model's memory.
    try {
      await configureSpeech(state);
      return;
    } catch (err) {
      log.error(`speech reconfigure failed: ${errMessage(err)}; restarting`);
    }
  }
  await sidecarManager().restart("speech", buildSpeechStartOptions(state));
  speechProcKey = procKey;
}

async function fileExists(path: string): Promise<boolean> {
  try {
    const st = await Deno.stat(path);
    return st.isFile;
  } catch {
    return false;
  }
}

function anyOverlap(a: ReadonlySet<string>, b: ReadonlySet<string>): boolean {
  for (const k of a) if (b.has(k)) return true;
  return false;
}

function logErr(kind: string) {
  return (err: unknown) => {
    log.error(`${kind} apply failed: ${errMessage(err)}`);
  };
}
