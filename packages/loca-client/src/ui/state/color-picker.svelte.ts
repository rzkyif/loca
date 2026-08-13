/**
 * Reactive store backing the global bubble-color picker. ColorField
 * pushes a request here when the user clicks its swatch; ColorPickerModal
 * (rendered alongside ConfirmModal in the settings Bubble) renders the
 * popup. Routing through state instead of mounting the popup inline
 * keeps it inside the click-through `contentEl` and inside the Bubble's
 * positioning context.
 */

type ColorPickerRequest = {
  /** Element the popup positions itself relative to. */
  anchor: HTMLElement;
  /** The color the popup opens with (an `oklch(...)` string, or legacy hex). */
  initialColor: string;
  /** Called when the user commits via the Apply button or Enter. */
  onApply: (hex: string) => void;
  /** When true, the lightness slider is hidden and L is pinned to 0.5. Used by
   *  "seed" colors (the default/accent/override bases) whose lightness the theme
   *  ladders ignore: only hue/chroma/alpha matter, so editing L would mislead. */
  lockLightness?: boolean;
};

class ColorPickerState {
  pending = $state<ColorPickerRequest | null>(null);

  open(req: ColorPickerRequest) {
    this.pending = req;
  }

  close() {
    this.pending = null;
  }

  apply(hex: string) {
    const p = this.pending;
    this.pending = null;
    p?.onApply(hex);
  }
}

export const colorPickerState = new ColorPickerState();
