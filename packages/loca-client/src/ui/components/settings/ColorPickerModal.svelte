<script lang="ts">
  import { colorPickerState } from "../../state";
  import { formatOklch, parseColor } from "$lib/appearance/color";
  import ColorPickerModalView from "@loca/shared/ui/components/settings/ColorPickerModalView.svelte";

  // Working OKLCHa state. Re-seeded each time a new request opens so
  // re-opening the same field doesn't carry leftover slider values.
  let l = $state(0);
  let c = $state(0);
  let h = $state(0);
  let a = $state(1);
  let previousColor = $state<string | null>(null);
  // Whether the user actually moved a slider since the picker opened. When
  // false, Apply returns the exact initial color so the field's no-op guard
  // sees an unchanged value and never rewrites the store.
  let dirty = $state(false);

  // Seed colors hide the lightness slider (their lightness is not rendered;
  // only hue/chroma/alpha seed the theme scale). L stays at the seeded value.
  const lockLightness = $derived(!!colorPickerState.pending?.lockLightness);

  let lastRequest: typeof colorPickerState.pending = null;
  $effect(() => {
    const p = colorPickerState.pending;
    if (p && p !== lastRequest) {
      lastRequest = p;
      const seed = parseColor(p.initialColor);
      // initialColor already carries the right lightness (locked by ColorField
      // when lockLightness is set); we just hide the L slider.
      l = seed.l;
      c = seed.c;
      h = seed.h;
      a = seed.a;
      previousColor = p.initialColor;
      dirty = false;
    }
    if (!p) lastRequest = null;
  });

  const newColor = $derived(formatOklch({ l, c, h, a }));

  // Apply the working color, or the untouched original when nothing changed.
  function apply() {
    colorPickerState.apply(dirty ? newColor : (previousColor ?? newColor));
  }

  // Enter commits. Popover already handles Esc-to-close.
  $effect(() => {
    if (!colorPickerState.pending) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        apply();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  });

  function setL(value: number) {
    l = value;
    dirty = true;
  }
  function setC(value: number) {
    c = value;
    dirty = true;
  }
  function setH(value: number) {
    h = value;
    dirty = true;
  }
  function setA(value: number) {
    a = value;
    dirty = true;
  }

  function reset() {
    if (!previousColor) return;
    const seed = parseColor(previousColor);
    l = seed.l;
    c = seed.c;
    h = seed.h;
    a = seed.a;
    // Back to the original: treat as unchanged so Apply is a clean no-op.
    dirty = false;
  }
</script>

<ColorPickerModalView
  open={!!colorPickerState.pending}
  anchor={colorPickerState.pending?.anchor}
  {l}
  {c}
  {h}
  {a}
  {lockLightness}
  {newColor}
  {previousColor}
  onChangeL={setL}
  onChangeC={setC}
  onChangeH={setH}
  onChangeA={setA}
  onApply={apply}
  onReset={reset}
  onClose={() => colorPickerState.close()}
/>
