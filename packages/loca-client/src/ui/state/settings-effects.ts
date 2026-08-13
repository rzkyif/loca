/**
 * Side-effect orchestrator for settingsState changes. Subscribes once and
 * drives the matching state mutations on other modules (VAD pause, TTS
 * toggle). LLM/STT sidecar restarts happen server-side now: PATCH
 * /api/v1/settings triggers core's own sidecar reconfiguration, so the
 * client only needs to handle the UI-side effects (VAD + TTS).
 */

import { platform } from "$lib/platform";
import { getLogger } from "$lib/util/log";
import { settingsState } from "./settings.svelte";
import { ttsState } from "./tts.svelte";
import { vadManager } from "./vad.svelte";

const log = getLogger("settings-effects");

settingsState.onChange((key, prev, next) => {
  if (key.startsWith("stt.")) {
    if (key === "stt.enabled" && prev !== false && !next) {
      // If the STT enable toggle just flipped off, also stop VAD.
      void vadManager.forceDisable();
    }
    if (key === "stt.autoSend" && next === true) {
      if (settingsState.currentSettings["stt.llmChainTranscription"]) {
        void settingsState.updateSetting("stt.llmChainTranscription", false);
      }
    } else if (key === "stt.llmChainTranscription" && next === true) {
      if (settingsState.currentSettings["stt.autoSend"]) {
        void settingsState.updateSetting("stt.autoSend", false);
      }
    }
  } else if (key === "tts.enabled" && !!prev !== !!next) {
    ttsState.setEnabled(!!next);
  } else if (key === "general.launch.autostart" && !!prev !== !!next) {
    // Mirror the toggle into the OS login entry.
    void platform()
      .autostart.setEnabled(!!next)
      .catch((e) => log.error("autostart update failed:", e));
  }
});
