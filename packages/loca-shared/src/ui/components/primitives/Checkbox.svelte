<script lang="ts">
  // The app's checkbox. A visually-hidden real <input> carries every
  // semantic (focus, form value, a11y, the indeterminate property, the test
  // harness); the box + glyph on top are plain elements driven by the reactive
  // props. The look is kept in sync with the display-only task-list checkboxes
  // rendered from markdown in MessageMarkdown.svelte: same box (bg-default-50,
  // 2px border-default-700, 0.25em radius) and the same bold check glyph.
  let {
    checked = false,
    indeterminate = false,
    disabled = false,
    onchange,
    ariaLabel,
    class: extraClass = "",
  }: {
    checked?: boolean;
    indeterminate?: boolean;
    disabled?: boolean;
    onchange?: (checked: boolean) => void;
    ariaLabel?: string;
    class?: string;
  } = $props();

  // `indeterminate` is an IDL property with no reflecting content attribute, so
  // it has to be assigned on the element rather than set as an attribute.
  let inputEl: HTMLInputElement | undefined = $state();
  $effect(() => {
    if (inputEl) inputEl.indeterminate = indeterminate;
  });
</script>

<span
  class="box-wrap relative inline-flex shrink-0 items-center justify-center {disabled
    ? 'opacity-60'
    : ''} {extraClass}"
>
  <input
    bind:this={inputEl}
    type="checkbox"
    aria-label={ariaLabel}
    class="absolute inset-0 m-0 opacity-0 {disabled ? '' : 'cursor-pointer'}"
    {checked}
    {disabled}
    onchange={(e) => onchange?.((e.target as HTMLInputElement).checked)}
  />
  <span
    aria-hidden="true"
    class="box size-[1.1em] flex items-center justify-center bg-default-50 border-2 border-default-700 rounded-[0.25em] text-[0.85em] font-bold leading-none"
  >
    {#if indeterminate}
      &minus;
    {:else if checked}
      &check;
    {/if}
  </span>
</span>

<style>
  /* The real input sits transparent over the box, so its focus ring is
     invisible; resurface keyboard focus on the box itself. */
  .box-wrap:has(:focus-visible) .box {
    outline: var(--focus-ring);
    outline-offset: var(--focus-ring-offset);
  }
</style>
