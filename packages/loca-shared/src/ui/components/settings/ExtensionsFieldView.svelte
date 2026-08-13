<script lang="ts">
  // Presentational empty-state for the extensions object manager. The client
  // owns the ObjectManager composition (list, cards, detail pane) and computes
  // which empty message applies from the live query; this View just renders the
  // matching title + hint. Pure: one data prop in, no callbacks.
  //
  //  - npmEmpty:     "@npm" with no query text yet (prompt to search)
  //  - npmNoResults: "@npm" with query text but no matches
  //  - noMatches:    installed filter has query text but no matches
  //  - none:         no query, nothing installed
  let {
    mode,
  }: {
    mode: "npmEmpty" | "npmNoResults" | "noMatches" | "none";
  } = $props();
</script>

<div class="flex flex-col items-center justify-center gap-1 py-12 text-center">
  {#if mode === "npmNoResults"}
    <div class="text-base text-default-700">No npm packages found</div>
  {:else if mode === "npmEmpty"}
    <div class="text-base text-default-700">Search the npm marketplace</div>
    <div class="text-sm text-default-500">Type a package name to search.</div>
  {:else if mode === "noMatches"}
    <div class="text-base text-default-700">No matching extensions</div>
  {:else}
    <div class="text-base text-default-700">No extensions installed</div>
    <div class="text-sm text-default-500">Add @npm to search the marketplace.</div>
  {/if}
</div>
