<script lang="ts">
  import type { Snippet } from "svelte";

  // Standard scrollable body for an object detail view. Renders the object's
  // description (when present) at the very top, then the type-specific content.
  // `pr-2` keeps content off the scrollbar (matches the list scroll in
  // ObjectManager). Used by every *Field detail snippet so the layout is
  // consistent regardless of object type.
  let { description, children }: { description?: string; children: Snippet } = $props();
</script>

<div class="loca-scroll flex-1 min-h-0 overflow-y-auto pr-2 flex flex-col gap-3">
  {#if description}
    <p class="text-sm text-default-600 break-words whitespace-pre-wrap">{description}</p>
  {/if}
  {@render children()}
</div>
