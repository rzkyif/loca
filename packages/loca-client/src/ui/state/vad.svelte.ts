/**
 * Manages the in-browser voice activity detector, the thing that listens
 * to the microphone and decides when the user is talking. Owns the VAD
 * model's lifecycle (load, start, pause, destroy), exposes its state for
 * the UI, and hands each detected speech segment to a caller-supplied
 * callback.
 */

import { platform } from "$lib/platform";
import { playBeep } from "$lib/stt/beep";
import { isTauri } from "$lib/util/env";
import { getLogger } from "$lib/util/log";
import { settingsState } from "./settings.svelte";
import { ttsState } from "./tts.svelte";

const log = getLogger("vad");

// Minimum gap between enable() retries after a failed init, so a repeating
// trigger (a held push-to-talk key, a component remount) can't call enable()
// several times a second - each retry would re-run getUserMedia and log the
// same failure (e.g. a missing mic permission), flooding the log.
const INIT_RETRY_COOLDOWN_MS = 3000;

class VadManager {
  enabled = $state(false);
  listening = $state(false);
  loading = $state(false);

  private instance: any = null;
  private pendingDisable = false;
  private pausedByHide = false;
  private unlistenVisibility: (() => void) | null = null;
  // Incremented on every enable() and detach(); any in-flight init whose token
  // doesn't match the current value is treated as superseded and cleaned up.
  private enableToken = 0;
  private onSpeech: ((audio: Float32Array) => Promise<void>) | null = null;
  // True iff this enable() call lowered the system volume and owes a restore.
  // The Rust side is the single source of truth (via saved_volume); this
  // flag just lets us avoid the IPC round-trip when no change was made.
  private volumeRestorePending = false;
  // Timestamp of the last failed enable(); gates the retry cooldown above. 0
  // means "no recent failure". Cleared on a successful enable().
  private initFailedAt = 0;

  /** Wire up the window-visibility listener and register the speech handler.
   *  Safe to call multiple times - re-attaching replaces the previous hook. */
  async attach(onSpeech: (audio: Float32Array) => Promise<void>): Promise<void> {
    this.onSpeech = onSpeech;
    if (this.unlistenVisibility) return;
    this.unlistenVisibility = await platform().windowing.subscribeVisibility(async (visible) => {
      if (!visible && this.enabled && this.instance) {
        if (settingsState.currentSettings["stt.activation"] === "sticky") {
          this.instance.pause();
          this.listening = false;
          this.pausedByHide = true;
        } else {
          await this.disableNow();
        }
      } else if (visible && this.pausedByHide && this.instance) {
        this.instance.start();
        this.pausedByHide = false;
      }
    });
  }

  detach() {
    this.enableToken++;
    if (this.instance) {
      this.instance.destroy();
      this.instance = null;
    }
    this.unlistenVisibility?.();
    this.unlistenVisibility = null;
    this.onSpeech = null;
    this.enabled = false;
    this.listening = false;
    this.loading = false;
    this.pendingDisable = false;
    this.pausedByHide = false;
    if (this.volumeRestorePending && isTauri()) {
      this.volumeRestorePending = false;
      // Best-effort: detach is sync-shaped here, but the platform call
      // returns a Promise. Fire and forget; the Rust-side Exit handler is
      // the safety net.
      void platform()
        .audio.restoreSystemVolume()
        .catch((e) => log.warn("restoreSystemVolume on detach failed:", e));
    }
  }

  /** User-facing toggle: if enabled, disables (deferring until current
   *  speech segment finishes); if disabled, enables. */
  async toggle(): Promise<void> {
    if (this.enabled) {
      if (this.listening && this.instance && !this.pendingDisable) {
        this.pendingDisable = true;
        return;
      }
      if (this.pendingDisable) return;
      await this.disableNow();
    } else {
      await this.enable();
    }
  }

  /** Force-disable immediately (no-op if not enabled). Unlike `toggle()`,
   *  doesn't defer for an in-progress speech segment. Used when STT is
   *  globally disabled. The speech sidecar may be stopped, so any segment
   *  still in the pipeline would just hit a dead endpoint. The visibility
   *  listener and speech handler stay attached so a later re-enable doesn't
   *  need a full UserInput remount. */
  async forceDisable(): Promise<void> {
    if (!this.enabled && !this.instance) return;
    await this.disableNow();
  }

  /** Decide whether voice input stays on after a speech segment (or misfire).
   *  Dictation is one-shot: a segment ends the session for manual mode and
   *  for a push-to-talk session started from the mic button. It stays on
   *  for sticky mode, while the push-to-talk shortcut is still held (pauses
   *  mid-dictation must not cut it off; release queues its own disable via
   *  `pendingDisable`), and while chain transcription wants more segments. */
  private async afterSegment(): Promise<void> {
    if (this.pendingDisable) {
      await this.disableNow();
      return;
    }
    const mode = settingsState.currentSettings["stt.activation"];
    if (mode === "sticky") return;
    if (settingsState.currentSettings["stt.llmChainTranscription"]) return;
    if (mode === "push-to-talk") {
      // Dynamic import: shortcut.svelte statically imports this module, so a
      // static import back would be a cycle.
      const { shortcutHandler } = await import("./shortcut.svelte");
      if (shortcutHandler.pttHolding) return;
    }
    await this.disableNow();
  }

  private async disableNow(): Promise<void> {
    if (this.instance) {
      this.instance.destroy();
      this.instance = null;
    }
    this.enabled = false;
    this.listening = false;
    this.pendingDisable = false;
    playBeep("off");
    if (settingsState.currentSettings["stt.activation"] === "sticky") {
      await settingsState.updateSetting("stt.vadPersistedState", false);
    }
    if (this.volumeRestorePending && isTauri()) {
      this.volumeRestorePending = false;
      try {
        await platform().audio.restoreSystemVolume();
      } catch (e) {
        log.warn("restoreSystemVolume failed:", e);
      }
    }
  }

  private async enable(): Promise<void> {
    // Throttle re-attempts after a recent failure so a repeating caller can't
    // hammer getUserMedia and flood the log (see INIT_RETRY_COOLDOWN_MS).
    if (this.initFailedAt && Date.now() - this.initFailedAt < INIT_RETRY_COOLDOWN_MS) {
      return;
    }
    const token = ++this.enableToken;
    this.loading = true;
    // Turning on voice input means the user wants to speak, not listen -
    // cut any TTS that's currently playing or queued.
    ttsState.reset();
    try {
      const { MicVAD } = await import("@ricky0123/vad-web");
      const instance = await MicVAD.new({
        baseAssetPath: "/vad/",
        onnxWASMBasePath: "/vad/",
        model: "v5",
        onSpeechStart: () => {
          this.listening = true;
        },
        onSpeechEnd: async (audio: Float32Array) => {
          this.listening = false;
          // Disable BEFORE handing the segment off: the audio is already
          // captured, and transcription + autocorrect can take a while, so
          // the mic (and its beep/icon) should reflect "done listening"
          // immediately instead of after the whole pipeline.
          await this.afterSegment();
          if (this.onSpeech) await this.onSpeech(audio);
        },
        onVADMisfire: () => {
          this.listening = false;
          void this.afterSegment();
        },
      });

      if (token !== this.enableToken) {
        instance.destroy();
        return;
      }

      this.instance = instance;
      this.instance.start();
      this.enabled = true;
      this.initFailedAt = 0;
      playBeep("on");
      if (settingsState.currentSettings["stt.activation"] === "sticky") {
        await settingsState.updateSetting("stt.vadPersistedState", true);
      }
      if (settingsState.currentSettings["stt.autoVolumeEnabled"] && isTauri()) {
        const target = Math.max(
          0,
          Math.min(100, Number(settingsState.currentSettings["stt.autoVolumeTarget"]) || 0),
        );
        try {
          await platform().audio.setSystemVolume(target);
          this.volumeRestorePending = true;
        } catch (e) {
          log.warn("setSystemVolume failed:", e);
        }
      }
    } catch (err) {
      log.error("Failed to initialize:", err);
      this.initFailedAt = Date.now();
      this.enabled = false;
    } finally {
      this.loading = false;
    }
  }
}

export const vadManager = new VadManager();
