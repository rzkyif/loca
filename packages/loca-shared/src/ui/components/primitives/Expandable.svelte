<script lang="ts">
  import type { Snippet } from "svelte";
  import Expand from "./Expand.svelte";
  import type { Alignment } from "../../types.ts";

  // A chevron-headed disclosure: a clickable title row that expands an animated
  // body. Shared so the client (message bubbles, settings rows) and the website
  // render the exact same disclosure.
  let {
    title,
    children,
    expanded = $bindable(false),
    disabled = false,
    alignment = "left",
    headerClass = "flex items-center gap-1 text-xs text-default-700 hov:cursor-pointer w-full",
  }: {
    title: Snippet;
    children: Snippet;
    /** Bindable open state. Parents own the source of truth so expansion can be
     *  driven externally (e.g. sharing into a global expansion map). */
    expanded?: boolean;
    /** Disables the toggle (empty-state gates like "no details to show"). */
    disabled?: boolean;
    /** Mirrors the bubble's alignment. When `right`, the chevron moves to the
     *  right end (and points left when collapsed), the header content is
     *  right-justified, and the body text is right-aligned. */
    alignment?: Alignment;
    headerClass?: string;
  } = $props();

  let isRight = $derived(alignment === "right");
  let chevronCollapsed = $derived(
    isRight
      ? "i-material-symbols-chevron-left-rounded"
      : "i-material-symbols-chevron-right-rounded",
  );
  let chevronIcon = $derived(
    expanded ? "i-material-symbols-keyboard-arrow-down-rounded" : chevronCollapsed,
  );
  // Pull the chevron a hair toward the bubble edge so the icon's optical center
  // sits flush: left pad on the left chevron, right pad on the right.
  let chevronMargin = $derived(isRight ? "-mr-0.5" : "-ml-0.5");
</script>

<div class="flex flex-col gap-2">
  <button
    class="{headerClass} {isRight ? 'justify-end text-right' : 'text-left'}"
    {disabled}
    onclick={() => (expanded = !expanded)}
    title={expanded ? "Collapse" : "Expand"}
  >
    {#if !isRight}
      <i class="flex transition-transform duration-200 {chevronMargin} {chevronIcon}"></i>
    {/if}
    {@render title()}
    {#if isRight}
      <i class="flex transition-transform duration-200 {chevronMargin} {chevronIcon}"></i>
    {/if}
  </button>
  <!-- `animateOnMount` mirrors the previous `in:expand|global`: the open
       animation runs even when the body mounts as a side effect of an ancestor
       mount (e.g. a stack regrouping the bubble into a standalone row). -->
  <Expand open={expanded} animateOnMount class={isRight ? "text-right" : ""}>
    {@render children()}
  </Expand>
</div>
