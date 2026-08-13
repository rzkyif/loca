<script lang="ts">
  // Presentational empty/error state for the MCP servers list. The list shell,
  // cards, and detail pane live in ../objects/* and ./McpDetailView; this is only
  // McpField's own empty-state markup. The client owns the live list and load
  // error and passes the resolved state in: a load error, an active search with
  // no matches, or no servers at all. Mirrors ToolsFieldView.
  import ErrorDetailView from "../chat/messages/ErrorDetailView.svelte";

  let {
    loadError = null,
    hasQuery = false,
  }: {
    loadError?: string | null;
    hasQuery?: boolean;
  } = $props();
</script>

{#if loadError}
  <div class="py-4">
    <ErrorDetailView message="Couldn't load MCP servers" detail={loadError} />
  </div>
{:else}
  <div class="flex flex-col items-center justify-center gap-1 py-12 text-center">
    {#if hasQuery}
      <div class="text-base text-default-700">No matching servers</div>
    {:else}
      <div class="text-base text-default-700">No MCP servers yet</div>
      <div class="text-sm text-default-500">Use the menu to add an MCP server.</div>
    {/if}
  </div>
{/if}
