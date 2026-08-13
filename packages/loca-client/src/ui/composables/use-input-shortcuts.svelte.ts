/**
 * The OS-level input-mode shortcuts (attach file, capture screen, capture
 * region) registered through tauri-plugin-global-shortcut. They are active only
 * while the composer is mounted AND the window is visible, so the consumer
 * (de)registers them in lockstep with its visibility subscription.
 *
 * Per the composable convention, the consumer owns the lifecycle: it keeps the
 * single window-visibility subscription (which also focuses the textarea) and
 * calls `register()`/`clear()` from it, subscribes the input-event stream to
 * its composer-backed handlers in onMount, and pushes the returned unsubscribe
 * into its own cleanup list. This class only owns the binding computation and
 * the platform calls, so it never reaches into the composer or send path.
 */

import { platform } from "$lib/platform";
import { settingsState } from "$stores/settings.svelte";
import { getLogger } from "$lib/util/log";

const log = getLogger("user-input");

export type InputShortcutHandlers = {
  onAttachFile: () => void;
  onCaptureScreen: () => void;
  onCaptureRegion: () => void;
};

export class InputShortcuts {
  private readInputBindings(): [string, string][] {
    const s = settingsState.currentSettings;
    return [
      ["attach-file", (s["shortcuts.attachFile"] as string) || ""],
      ["capture-screen", (s["shortcuts.captureScreen"] as string) || ""],
      ["capture-region", (s["shortcuts.captureRegion"] as string) || ""],
    ];
  }

  async register(): Promise<void> {
    try {
      await platform().shortcuts.setInputBindings(this.readInputBindings());
    } catch (e) {
      log.warn("set_input_shortcuts failed:", e);
    }
  }

  async clear(): Promise<void> {
    try {
      await platform().shortcuts.setInputBindings([]);
    } catch (e) {
      log.warn("clear input_shortcuts failed:", e);
    }
  }

  // Subscribe the OS input-event stream to the given handlers; returns the
  // unsubscribe for the consumer's cleanup list.
  subscribeEvents(handlers: InputShortcutHandlers): Promise<() => void> {
    return platform().shortcuts.subscribeInputEvents(handlers);
  }

  // Initial registration: only if the window is already visible at mount time.
  // Most of the time it is (closing Settings doesn't hide the window), but on
  // app startup with a hidden tray-only launch we'd otherwise register
  // shortcuts that should be inactive.
  async registerIfVisible(): Promise<void> {
    try {
      const visible = await platform().windowing.isVisible();
      if (visible) await this.register();
    } catch (e) {
      log.warn("initial visibility check failed:", e);
    }
  }
}
