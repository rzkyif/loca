// Idle-unload supervisor for the local llama-server.
//
// When `llm.idleUnloadSeconds > 0` (enabled automatically by the Full preset),
// the model is stopped after that many seconds with no active chat turns, freeing
// its memory, and transparently reloaded on the next turn. This is a loca-side
// mechanism: llama-server has no native idle unload.
//
// Lifecycle, driven by ChatService:
//   - noteActivity()  at the start of every turn  -> cancel any pending unload
//   - ensureLoaded()  before a local turn is sent -> reload if we unloaded it
//   - onTurnEnd(n)     when a turn finishes        -> arm the timer iff idle
//
// It talks to the SidecarManager directly (not applyLlama) to avoid a cycle with
// sidecar-boot, which itself imports this module.

import { sidecarManager } from "../sidecars/manager.ts";
import {
  buildLlamaStartOptions,
  llamaMissingPrereq,
  llamaStartArgsFromSettings,
} from "../sidecars/llama.ts";
import { getLogger } from "../shared/log.ts";
import { numSetting } from "@loca/core-engine/services/settings-access";
import { errMessage } from "@loca/shared";

const log = getLogger("llm-idle");

class LlmIdleManager {
  private timer: ReturnType<typeof setTimeout> | null = null;
  private idleSeconds = 0;

  /** Re-read the idle setting (called on boot + on settings change). Disabling
   *  it cancels any pending unload. */
  configure(settings: Record<string, unknown>): void {
    this.idleSeconds = numSetting(settings, "llm.idleUnloadSeconds", 0);
    if (this.idleSeconds <= 0) this.cancelTimer();
  }

  /** A turn is starting: never unload while busy. */
  noteActivity(): void {
    this.cancelTimer();
  }

  /** Ensure the local model is loaded before a turn is sent. No-op for external
   *  providers, when llama is already running/starting, or when a required file
   *  (model, mmproj, binary) is not on disk yet - spawning anyway would flip the
   *  sidecar (and the corebar) to Error while a download is still in flight. */
  async ensureLoaded(settings: Record<string, unknown>): Promise<void> {
    const args = llamaStartArgsFromSettings(settings);
    if (!args) return; // external provider or no model configured
    const status = sidecarManager().status("llama").status;
    if (status === "Running" || status === "Loading") return;
    const missing = await llamaMissingPrereq(args);
    if (missing) {
      log.info(`llama-server not reloaded: ${missing}`);
      return;
    }
    log.info("reloading llama-server after idle unload");
    try {
      await sidecarManager().start("llama", buildLlamaStartOptions(args));
    } catch (err) {
      log.error(`idle reload failed: ${errMessage(err)}`);
    }
  }

  /** A turn finished. Arm the unload timer when idle-unload is on and nothing
   *  else is in flight. */
  onTurnEnd(activeCount: number): void {
    if (this.idleSeconds <= 0 || activeCount > 0) return;
    this.cancelTimer();
    const seconds = this.idleSeconds;
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.unload(seconds);
    }, seconds * 1000);
  }

  private async unload(seconds: number): Promise<void> {
    try {
      await sidecarManager().stop("llama");
      log.info(`idle-unloaded llama-server after ${seconds}s of inactivity`);
    } catch (err) {
      log.warn(`idle unload failed: ${errMessage(err)}`);
    }
  }

  /** Cancel any pending unload timer. Called on shutdown so the timer can't keep
   *  the event loop alive (under dev `--watch` that would stall the restart). */
  dispose(): void {
    this.cancelTimer();
  }

  private cancelTimer(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }
}

let _instance: LlmIdleManager | null = null;
export function llmIdle(): LlmIdleManager {
  if (!_instance) _instance = new LlmIdleManager();
  return _instance;
}
