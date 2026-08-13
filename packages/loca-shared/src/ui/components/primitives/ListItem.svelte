<script lang="ts">
  import type { Snippet } from "svelte";

  type Direction = "col" | "row";

  let {
    selected = false,
    disabled = false,
    onclick,
    onmousedown,
    direction = "col",
    leading,
    trailing,
    children,
    role,
    ariaSelected,
    class: extraClass = "",
  }: {
    selected?: boolean;
    disabled?: boolean;
    onclick?: () => void;
    onmousedown?: (e: MouseEvent) => void;
    /** Inner content layout. `col` (default) stacks children vertically;
     *  fits a title-above-summary list row. `row` flexes them horizontally;
     *  fits a key-value option row (e.g. autocomplete dropdown). */
    direction?: Direction;
    leading?: Snippet;
    trailing?: Snippet;
    children: Snippet;
    role?: string;
    ariaSelected?: boolean;
    class?: string;
  } = $props();

  const interactive = $derived(!!(onclick || onmousedown));

  const surfaceClass = $derived(
    selected
      ? "bg-surface-inset"
      : interactive
        ? "bg-transparent hover:bg-surface-inset"
        : "bg-transparent",
  );

  const contentLayoutClass = $derived(
    direction === "row" ? "flex flex-row items-center gap-3" : "flex flex-col",
  );
</script>

<div
  class="flex items-center gap-2 rounded-large px-3 py-2 transition-colors {surfaceClass} {disabled
    ? 'opacity-60 pointer-events-none'
    : ''} {extraClass}"
  {role}
  aria-selected={ariaSelected}
>
  {#if leading}
    {@render leading()}
  {/if}
  {#if onclick || onmousedown}
    <button
      type="button"
      class="{contentLayoutClass} min-w-0 flex-1 text-left hover:cursor-pointer"
      {disabled}
      {onclick}
      {onmousedown}
    >
      {@render children()}
    </button>
  {:else}
    <div class="{contentLayoutClass} min-w-0 flex-1 text-left">
      {@render children()}
    </div>
  {/if}
  {#if trailing}
    {@render trailing()}
  {/if}
</div>
