<script lang="ts">
  import type { Snippet } from "svelte";

  // A detail-view subsection micro-header: a tiny uppercase label with optional
  // trailing controls (e.g. an add button). Smaller than SectionHeader, which is
  // the larger sticky section/group header of the main settings panel. Shared so
  // client and website render detail subsections identically.
  let {
    label,
    actions,
    class: extraClass = "",
  }: {
    label: string;
    /** Trailing controls aligned to the right (e.g. an add IconButton). */
    actions?: Snippet;
    class?: string;
  } = $props();
</script>

<div class="flex items-center gap-2 {extraClass}">
  <div class="text-default-400 text-[10px] uppercase tracking-wider select-none flex-1">
    {label}
  </div>
  {#if actions}
    {@render actions()}
  {/if}
</div>
