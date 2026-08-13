/**
 * Plays short beep sounds to give the user audio feedback for things like
 * the microphone turning on or off.
 */

import { getLogger } from "$lib/util/log";

const log = getLogger("beep");

// Safari historically only exposed AudioContext as `webkitAudioContext`.
// Modern Safari ships the unprefixed name too, but the older one is still
// the only path for some legacy macOS / iOS builds. Declare the optional
// vendor-prefixed constructor on Window via an ambient extension so the
// fallback compiles without `any`.
interface WindowWithLegacyAudio extends Window {
  webkitAudioContext?: typeof AudioContext;
}

let audioCtx: AudioContext | null = null;

function ctx(): AudioContext {
  if (!audioCtx) {
    const w = window as WindowWithLegacyAudio;
    const Ctor = window.AudioContext ?? w.webkitAudioContext;
    if (!Ctor) {
      throw new Error("Web Audio API is not available in this browser");
    }
    audioCtx = new Ctor();
  }
  return audioCtx;
}

/**
 * Play a short sine-wave ping. "on" uses a higher pitch (880 Hz),
 * "off" uses a lower one (440 Hz). Silently no-ops if Web Audio isn't available.
 */
export function playBeep(type: "on" | "off"): void {
  try {
    const c = ctx();
    if (c.state === "suspended") {
      c.resume().catch(() => {});
    }
    const osc = c.createOscillator();
    const gain = c.createGain();
    osc.type = "sine";
    osc.frequency.value = type === "on" ? 880 : 440;
    const now = c.currentTime;
    gain.gain.setValueAtTime(0.0001, now);
    gain.gain.exponentialRampToValueAtTime(0.15, now + 0.01);
    gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.15);
    osc.connect(gain).connect(c.destination);
    osc.start(now);
    osc.stop(now + 0.16);
  } catch (e) {
    log.warn("Failed to play beep:", e);
  }
}
