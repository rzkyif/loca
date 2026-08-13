<script lang="ts">
  // Presentational OKLCHa color picker: four sliders (lightness, chroma, hue,
  // alpha) anchored in a popover, a new-vs-previous swatch, and reset/apply
  // buttons. All values arrive as plain data (the client owns the working state
  // and computes `newColor` via the color helpers), so this stays pure: props
  // in, callbacks out.
  import Popover from "../primitives/Popover.svelte";
  import Button from "../primitives/Button.svelte";

  let {
    open,
    anchor,
    l,
    c,
    h,
    a,
    lockLightness = false,
    newColor,
    previousColor = null,
    onChangeL = noop,
    onChangeC = noop,
    onChangeH = noop,
    onChangeA = noop,
    onApply = noop,
    onReset = noop,
    onClose = noop,
  }: {
    open: boolean;
    anchor?: HTMLElement | null;
    l: number;
    c: number;
    h: number;
    a: number;
    lockLightness?: boolean;
    newColor: string;
    previousColor?: string | null;
    onChangeL?: (value: number) => void;
    onChangeC?: (value: number) => void;
    onChangeH?: (value: number) => void;
    onChangeA?: (value: number) => void;
    onApply?: () => void;
    onReset?: () => void;
    onClose?: () => void;
  } = $props();

  function noop(): void {}

  const numberFromEvent = (e: Event): number => parseFloat((e.target as HTMLInputElement).value);
</script>

<Popover {open} {anchor} onclose={onClose} class="w-72" ariaLabel="Color picker">
  <div class="grid grid-cols-[1.25rem_1fr_3rem] items-center gap-x-2 gap-y-2">
    {#if !lockLightness}
      <span class="text-default-700 text-sm">L</span>
      <input
        type="range"
        min="0"
        max="1"
        step="0.001"
        value={l}
        oninput={(e) => onChangeL(numberFromEvent(e))}
        class="w-full"
        aria-label="Lightness"
      />
      <span class="text-default-600 text-xs text-right tabular-nums">
        {l.toFixed(3)}
      </span>
    {/if}

    <span class="text-default-700 text-sm">C</span>
    <input
      type="range"
      min="0"
      max="0.4"
      step="0.001"
      value={c}
      oninput={(e) => onChangeC(numberFromEvent(e))}
      class="w-full"
      aria-label="Chroma"
    />
    <span class="text-default-600 text-xs text-right tabular-nums">
      {c.toFixed(3)}
    </span>

    <span class="text-default-700 text-sm">H</span>
    <input
      type="range"
      min="0"
      max="360"
      step="1"
      value={h}
      oninput={(e) => onChangeH(numberFromEvent(e))}
      class="w-full"
      aria-label="Hue"
    />
    <span class="text-default-600 text-xs text-right tabular-nums">
      {h.toFixed(0)}°
    </span>

    <span class="text-default-700 text-sm">A</span>
    <input
      type="range"
      min="0"
      max="1"
      step="0.01"
      value={a}
      oninput={(e) => onChangeA(numberFromEvent(e))}
      class="w-full"
      aria-label="Alpha"
    />
    <span class="text-default-600 text-xs text-right tabular-nums">
      {a.toFixed(2)}
    </span>
  </div>

  <div class="alpha-checkerboard rounded-medium overflow-hidden grid grid-rows-2 h-20">
    <div style:background-color={newColor} title="New"></div>
    {#if previousColor}
      <div style:background-color={previousColor} title="Previous"></div>
    {/if}
  </div>

  <div class="flex flex-row items-center gap-2">
    <Button variant="secondary" class="flex-1" onclick={onReset}>Reset</Button>
    <Button variant="secondary" class="flex-1" onclick={onApply}>Apply</Button>
  </div>
</Popover>
