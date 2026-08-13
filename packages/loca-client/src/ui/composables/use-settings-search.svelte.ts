/**
 * Search-mode state + a reusable slide-swap animation for the settings panel's
 * content layer. `slideSwap` runs a sequential transition: phase 1 slides the
 * current content out, phase 2 swaps it while offscreen on the opposite side,
 * phase 3 slides the new content in. It drives both entering/exiting search
 * and switching between setting groups (Settings.svelte computes the direction
 * from group order).
 *
 * "up" = current content leaves upward, new content enters from below (entering
 * search, or moving to an earlier group); "down" is the reverse.
 */

import { tick } from "svelte";
import { getDuration } from "$lib/appearance/animations";
import { slideSwap } from "@loca/shared/ui/animations";

export class SettingsSearch {
  mode = $state(false);
  query = $state("");
  layerEl: HTMLDivElement | undefined = $state();
  inputEl: HTMLInputElement | undefined = $state();
  /** When set, the slide is delegated to the owner (the shared
   *  `SettingsShellView`, which owns the content layer); the local
   *  `layerEl` slide path below is then only used by tests / standalone. */
  onSlide: ((active: boolean) => Promise<void> | void) | undefined;

  private transitioning = false;

  /**
   * Slide the layer out in direction `dir`, run `swap` while it's offscreen on
   * the opposite side, then slide the new content in. With no layer attached
   * (tests) or animations disabled, `swap` runs synchronously and we return.
   */
  async slideSwap(dir: "up" | "down", swap: () => void): Promise<void> {
    if (this.transitioning) return;
    this.transitioning = true;
    // Vertical slide; "up" means the current layer leaves upward (outSign 1).
    // Await `tick()` inside the swap so the new content exists before the
    // shared helper parks it offscreen for the entry phase.
    await slideSwap(this.layerEl, {
      axis: "y",
      outSign: dir === "up" ? 1 : -1,
      durationMs: getDuration(),
      swap: async () => {
        swap();
        await tick();
      },
    });
    this.transitioning = false;
  }

  async setMode(active: boolean): Promise<void> {
    if (this.mode === active) return;
    if (this.onSlide) {
      // The shell owns the slide; it also flips `mode` through its bound prop.
      await this.onSlide(active);
      this.mode = active;
      return;
    }
    await this.slideSwap(active ? "up" : "down", () => {
      this.mode = active;
    });
  }

  onInput(): void {
    if (this.query.trim()) {
      void this.setMode(true);
    } else if (this.mode) {
      void this.setMode(false);
    }
  }

  clear(): void {
    this.query = "";
    void this.setMode(false);
    // preventScroll: never let refocusing the field scroll an off-screen panel
    // into view (see the mount autofocus in Settings.svelte).
    this.inputEl?.focus({ preventScroll: true });
  }
}

export function useSettingsSearch(): SettingsSearch {
  return new SettingsSearch();
}
