/**
 * App-level view mode. The client renders one of five top-level modes; this
 * store holds which one and exposes a request API for switching between them.
 *
 * The slide transition in +page.svelte is "slide out → swap content → slide
 * in", so the mode the user actually sees (`mode`) is committed at the
 * OFFSCREEN midpoint, not when the request is made. Components call
 * `navigate(next)`, which sets `pendingMode`; +page.svelte watches
 * `pendingMode`, runs the slide, and calls `commit()` mid-slide. The store
 * itself stays DOM-free. The panel element it animates lives in +page.svelte.
 */

import { ttsState } from "./tts.svelte";

export type AppMode = "newCore" | "quickSettings" | "chat" | "sessionList" | "settings";

class ViewState {
  /** The mode currently rendered. Written only by `commit()` (at the slide's
   *  offscreen midpoint) and by `setImmediate()` (the no-animation boot path). */
  mode = $state<AppMode>("chat");
  /** The requested mode. +page.svelte's $effect drives the slide toward it. */
  pendingMode = $state<AppMode>("chat");
  /** True while no core is paired: navigation away from newCore is blocked so
   *  the rest of the UI (which would call `cores().api()`) can't be reached. */
  locked = $state(false);
  /** A settings group id Settings should open on its next mount, then clear.
   *  Lets the add-core flow return the user to the Cores manager. */
  pendingSettingsGroup = $state<string | null>(null);

  /** Request a transition to `next`. No-ops while locked (unless the target
   *  is newCore) or when that mode is already pending. */
  navigate(next: AppMode): void {
    if (this.locked && next !== "newCore") return;
    if (this.pendingMode === next) return;
    // Speech belongs to the chat view: cut any current/queued TTS when
    // leaving it. streamingState.feedTTS holds new sentences back while
    // pendingMode is non-chat, so a running stream can't re-arm it.
    if (next !== "chat") ttsState.reset();
    this.pendingMode = next;
  }

  /** Commit the pending mode. Called by +page.svelte at the offscreen
   *  midpoint of the slide so the content swap is never visible. */
  commit(): void {
    this.mode = this.pendingMode;
  }

  /** Jump straight to a mode with no animation. Used by the boot path. */
  setImmediate(next: AppMode): void {
    this.mode = next;
    this.pendingMode = next;
  }

  setLocked(value: boolean): void {
    this.locked = value;
    // Entering the locked state means no usable core remains, so force the
    // only reachable mode. Otherwise removing the last core from a core-backed
    // mode (e.g. Settings) would strand the UI there with no selected core,
    // and every cores().api() call would throw. navigate() no-ops when
    // newCore is already pending (the boot path is already there).
    if (value) this.navigate("newCore");
  }
}

export const viewState = new ViewState();
