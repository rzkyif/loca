<script lang="ts">
  import Input from "./Input.svelte";

  // A range slider with an optional paired number input and suffix. Shared so
  // client and website render sliders identically (range chrome lives in
  // base.css). The paired number tracks the thumb live while dragging.
  let {
    value,
    min,
    max,
    step = 1,
    oninput,
    onchange,
    disabled = false,
    pairedInput = false,
    suffix,
    ariaLabel,
    error = false,
    class: extraClass = "",
  }: {
    value: number;
    min: number;
    max: number;
    step?: number;
    oninput?: (v: number) => void;
    onchange?: (v: number) => void;
    disabled?: boolean;
    pairedInput?: boolean;
    suffix?: string;
    ariaLabel?: string;
    error?: boolean;
    class?: string;
  } = $props();

  // Live value shown while dragging; cleared whenever the committed `value`
  // changes (commit landed / reset / external update) so we fall back to truth.
  let dragValue = $state<number | undefined>(undefined);
  $effect(() => {
    void value;
    dragValue = undefined;
  });
  const shown = $derived(dragValue ?? value);

  function commit(raw: string | number, kind: "input" | "change") {
    const n = typeof raw === "number" ? raw : parseFloat(raw);
    if (Number.isNaN(n)) return;
    const clamped = Math.max(min, Math.min(max, n));
    if (kind === "input") {
      dragValue = clamped;
      oninput?.(clamped);
    } else {
      onchange?.(clamped);
    }
  }
</script>

<div
  class="flex flex-row items-center gap-3 w-full {disabled
    ? 'opacity-60 pointer-events-none'
    : ''} {extraClass}"
>
  <input
    type="range"
    aria-label={ariaLabel}
    class="loca-slider flex-1 min-w-0"
    {min}
    {max}
    {step}
    value={shown}
    {disabled}
    oninput={(e) => commit((e.target as HTMLInputElement).value, "input")}
    onchange={(e) => commit((e.target as HTMLInputElement).value, "change")}
  />
  {#if pairedInput}
    <div class="w-16 shrink-0">
      <Input
        type="number"
        value={shown}
        {min}
        {max}
        {step}
        {disabled}
        {error}
        {ariaLabel}
        onchange={(v) => commit(v, "change")}
      />
    </div>
  {/if}
  {#if suffix}
    <span class="text-default-500 text-sm shrink-0">{suffix}</span>
  {/if}
</div>
