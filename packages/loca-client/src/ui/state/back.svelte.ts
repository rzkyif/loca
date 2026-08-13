/**
 * Mobile back-navigation registry. The mobile platform owns the back gesture
 * (Android's hardware/system back, with TauriActivity shipping
 * `handleBackNavigation = false`, or an iOS left-edge swipe), so a single
 * subscription in +page.svelte feeds every press to `back()`, which resolves one
 * step of a priority chain that mirrors Android's `OnBackPressedDispatcher`:
 *
 *   1. an open overlay (Modal / Popover / ActionSheet) closes,
 *   2. the pairing wizard steps back a substep,
 *   3. any non-chat top-level mode returns to chat,
 *   4. at the chat root, a double-back exits (first press hints, a second
 *      within the window leaves the app) - only where the platform can leave
 *      (backButton.canExit): Android. On iOS the root back is inert (the OS home
 *      gesture owns leaving); on desktop the stream never fires.
 *
 * Steps 1-2 are interceptors components push onto a LIFO stack (overlays via the
 * UiContext `registerBack` bridge so the shared primitives never import this
 * client store; the wizard directly). Steps 3-4 are the fallback when no
 * interceptor consumes the press. Esc-to-close on overlays stays their own concern.
 */

import { platform } from "$lib/platform";
import { viewState } from "./view.svelte";

/** How long after the first root back a second press still exits. */
const EXIT_WINDOW_MS = 2000;

class BackState {
  /** LIFO interceptor stack. Each returns true when it consumed the press. */
  #handlers: Array<() => boolean> = [];
  /** Armed for ~2s after the first back at the chat root; a second back exits.
   *  Reactive so the mobile shell can surface a "press back again" hint. */
  exitHint = $state(false);
  #exitTimer: ReturnType<typeof setTimeout> | undefined;

  /** Push a back interceptor; returns a disposer to pop it. */
  push(handler: () => boolean): () => void {
    this.#handlers.push(handler);
    return () => {
      const i = this.#handlers.lastIndexOf(handler);
      if (i >= 0) this.#handlers.splice(i, 1);
    };
  }

  /** Resolve one back press through the priority chain. */
  back(): void {
    // 1 + 2: the most recently registered interceptor (topmost overlay, then
    // wizard substep) gets first refusal.
    for (let i = this.#handlers.length - 1; i >= 0; i--) {
      if (this.#handlers[i]()) {
        this.#disarmExit();
        return;
      }
    }
    // 3: a non-chat top-level mode returns to chat. While locked (no core
    // paired: only newCore is reachable) navigation is blocked, so a locked
    // newCore root falls through to the exit chain below instead.
    if (viewState.pendingMode !== "chat" && !viewState.locked) {
      viewState.navigate("chat");
      this.#disarmExit();
      return;
    }
    // 4: chat root (or a locked newCore root) - double-back-to-exit.
    this.#requestExit();
  }

  #requestExit(): void {
    // Only where a root back leaves the app (Android) does the double-back
    // ceremony make sense. On iOS the OS home gesture owns leaving, so a root
    // edge-swipe is inert rather than showing a hint whose second tap no-ops.
    if (!platform().backButton.canExit()) return;
    if (this.exitHint) {
      this.#disarmExit();
      void platform().backButton.exit();
      return;
    }
    this.exitHint = true;
    this.#exitTimer = setTimeout(() => {
      this.exitHint = false;
      this.#exitTimer = undefined;
    }, EXIT_WINDOW_MS);
  }

  #disarmExit(): void {
    if (this.#exitTimer !== undefined) {
      clearTimeout(this.#exitTimer);
      this.#exitTimer = undefined;
    }
    this.exitHint = false;
  }
}

export const backState = new BackState();
