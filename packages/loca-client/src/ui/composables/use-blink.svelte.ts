/**
 * Drives a boolean that toggles on a fixed interval while `active` is true, for
 * the "attention ping": a button swaps between two design-token color classes
 * and transition-colors tweens the change into a brightness/hue pulse.
 *
 * Pass an `intervalMs` equal to the element's CSS transition duration so each
 * toggle fires exactly when the tween finishes: the color is always in motion
 * (a smooth triangle wave) with no flat hold at either extreme.
 *
 * A JS interval rather than a CSS keyframe so the pulse can ride design tokens
 * (CSS vars with dark-mode variants) instead of hardcoding oklch values and
 * restating the dark variant. Convention mirrors use-responsive-layout: the
 * consuming component owns the $effect (see Settings.svelte's layout.observe).
 */
export class Blink {
  on = $state(false);
  private intervalMs: number;

  constructor(intervalMs = 500) {
    this.intervalMs = intervalMs;
  }

  /** Call inside a `$effect` that reads the reactive condition, e.g.
   *  `$effect(() => blink.run(state.hasPending))`. Returns the effect teardown. */
  run(active: boolean): () => void {
    if (!active) {
      this.on = false;
      return () => {};
    }
    const id = setInterval(() => {
      this.on = !this.on;
    }, this.intervalMs);
    return () => clearInterval(id);
  }
}

export function useBlink(intervalMs = 500): Blink {
  return new Blink(intervalMs);
}
