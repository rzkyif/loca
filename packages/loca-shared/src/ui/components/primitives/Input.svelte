<script lang="ts">
  import type { Snippet } from "svelte";

  // A text/number/password field with optional prefix, suffix, and numeric
  // spinner. Shared so client and website render inputs identically.
  type Type = "text" | "number" | "password";

  let {
    type = "text",
    value,
    oninput,
    onchange,
    onfocus,
    onblur,
    onkeydown,
    onclick,
    placeholder,
    disabled = false,
    error = false,
    spinner = false,
    step,
    min,
    max,
    maxlength,
    prefix,
    suffix,
    trailing,
    mono = false,
    uppercase = false,
    spellcheck,
    autocomplete,
    ariaLabel,
    class: extraClass = "",
    el = $bindable<HTMLInputElement | undefined>(undefined),
  }: {
    type?: Type;
    value: string | number | undefined;
    oninput?: (v: string, e: Event) => void;
    onchange?: (v: string, e: Event) => void;
    onfocus?: (e: FocusEvent) => void;
    onblur?: (e: FocusEvent) => void;
    onkeydown?: (e: KeyboardEvent) => void;
    onclick?: (e: MouseEvent) => void;
    placeholder?: string;
    disabled?: boolean;
    error?: boolean;
    spinner?: boolean;
    step?: number | string;
    min?: number;
    max?: number;
    maxlength?: number;
    prefix?: Snippet;
    suffix?: string;
    /** Interactive adornment pinned inside the field at the right edge (e.g. a
     *  small IconButton), like the number spinner. The input reserves right
     *  padding so its text never runs under the control. */
    trailing?: Snippet;
    mono?: boolean;
    uppercase?: boolean;
    spellcheck?: boolean;
    autocomplete?: "on" | "off";
    ariaLabel?: string;
    class?: string;
    el?: HTMLInputElement | undefined;
  } = $props();

  const isNumber = $derived(type === "number");
  // Errored fields keep the neutral inset fill and contents; the invalid state
  // reads as a red inset outline instead (the `loca-error-ring` helper draws
  // the resting red outline and keeps the keyboard-focus ring red too).
  const stateClass = $derived(error ? "bg-surface-inset loca-error-ring" : "bg-surface-inset");
  const fontClass = $derived(`${mono ? "font-mono" : ""} ${uppercase ? "uppercase" : ""}`);

  function adjust(direction: 1 | -1) {
    if (disabled) return;
    const stepNum = typeof step === "string" ? parseFloat(step) : (step ?? 1);
    const base =
      typeof value === "number"
        ? value
        : typeof value === "string" && value !== ""
          ? parseFloat(value)
          : 0;
    if (Number.isNaN(base)) return;
    let next = base + direction * stepNum;
    if (typeof min === "number") next = Math.max(min, next);
    if (typeof max === "number") next = Math.min(max, next);
    // Avoid float artifacts for sub-integer steps.
    if (stepNum < 1) {
      const decimals = Math.max(0, -Math.floor(Math.log10(stepNum)));
      next = Number(next.toFixed(decimals));
    }
    const str = String(next);
    onchange?.(str, new Event("change"));
  }
</script>

<div
  class="flex flex-row items-center gap-2 w-full {disabled ? 'opacity-60 pointer-events-none' : ''}"
>
  <div class="relative flex-1 min-w-0">
    {#if prefix}
      <span
        class="absolute left-2 top-1/2 -translate-y-1/2 text-default-500 pointer-events-none flex items-center"
      >
        {@render prefix()}
      </span>
    {/if}
    <input
      bind:this={el}
      {type}
      {step}
      {min}
      {max}
      {maxlength}
      {placeholder}
      {disabled}
      {spellcheck}
      {autocomplete}
      aria-label={ariaLabel}
      value={value ?? ""}
      oninput={(e) => oninput?.((e.target as HTMLInputElement).value, e)}
      onchange={(e) => onchange?.((e.target as HTMLInputElement).value, e)}
      {onfocus}
      {onblur}
      {onkeydown}
      {onclick}
      class="text-default-800 rounded-medium block w-full min-h-8 outline-none text-sm {isNumber
        ? 'loca-no-spinner'
        : ''} {prefix ? 'pl-6' : 'pl-2'} {trailing
        ? 'pr-9'
        : spinner
          ? 'pr-7'
          : 'pr-2'} {fontClass} {stateClass} {extraClass}"
    />
    {#if spinner && isNumber}
      <div class="absolute right-1 top-1/2 -translate-y-1/2 flex flex-col">
        <button
          type="button"
          tabindex={-1}
          aria-label="Increase"
          class="text-default-500 hov:text-default-800 act:text-default-900 transition-interactive disabled:opacity-40 disabled:cursor-not-allowed flex items-center justify-center h-3.5 w-5"
          {disabled}
          onclick={() => adjust(1)}
        >
          <i class="i-material-symbols-keyboard-arrow-up-rounded text-base flex"></i>
        </button>
        <button
          type="button"
          tabindex={-1}
          aria-label="Decrease"
          class="text-default-500 hov:text-default-800 act:text-default-900 transition-interactive disabled:opacity-40 disabled:cursor-not-allowed flex items-center justify-center h-3.5 w-5"
          {disabled}
          onclick={() => adjust(-1)}
        >
          <i class="i-material-symbols-keyboard-arrow-down-rounded text-base flex"></i>
        </button>
      </div>
    {/if}
    {#if trailing}
      <div class="absolute right-1 top-1/2 -translate-y-1/2 flex items-center">
        {@render trailing()}
      </div>
    {/if}
  </div>
  {#if suffix}
    <span class="text-default-500 text-sm shrink-0">{suffix}</span>
  {/if}
</div>

<style>
  :global(.loca-no-spinner::-webkit-inner-spin-button),
  :global(.loca-no-spinner::-webkit-outer-spin-button) {
    -webkit-appearance: none;
    margin: 0;
  }
  :global(.loca-no-spinner) {
    -moz-appearance: textfield;
    appearance: textfield;
  }
</style>
