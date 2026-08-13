<script lang="ts">
  import ExpandableMessageView from "./ExpandableMessageView.svelte";
  import ErrorDetailView from "./ErrorDetailView.svelte";

  // The "Found N relevant memories" bubble: the shared collapsed-message shell
  // with a body listing each matched memory and its score. The client wrapper
  // maps the message's `relevant` array onto `memories`; this stays presentational.
  export type RelevantMemory = {
    memoryId?: string;
    title: string;
    score: number;
    summary?: string;
  };

  let {
    id,
    memories = [],
    status = "complete",
    errorMessage,
    defaultExpanded = false,
    neighborLeft = false,
    neighborRight = false,
  }: {
    id?: string;
    memories?: RelevantMemory[];
    status?: string;
    errorMessage?: string;
    defaultExpanded?: boolean;
    neighborLeft?: boolean;
    neighborRight?: boolean;
  } = $props();

  const count = $derived(memories.length);
  const titleText = $derived.by(() => {
    if (status === "error") return "Failed to find relevant memories";
    if (count === 0) return "No relevant memories";
    return `Found ${count} relevant memor${count === 1 ? "y" : "ies"}`;
  });

  const formatScore = (s: number): string => s.toFixed(2);
</script>

<ExpandableMessageView
  {id}
  title={titleText}
  applyColorOverride
  {defaultExpanded}
  {neighborLeft}
  {neighborRight}
>
  {#snippet body()}
    <div class="flex flex-col gap-2 text-xs text-left">
      {#if errorMessage}
        <ErrorDetailView message="Couldn't search your memories" detail={errorMessage} />
      {/if}

      {#if count === 0}
        <div class="text-default-500 italic px-3 py-2">No memories matched.</div>
      {:else}
        <div class="flex flex-col gap-1">
          {#each memories as memory, di (memory.memoryId ?? di)}
            <div
              class="bg-surface-inset rounded-large px-4 py-2 text-default-800 whitespace-pre-wrap break-words"
            >
              <span class="font-bold">{memory.title}</span><span
                class="text-default-500 tabular-nums font-mono"
                >&nbsp;({formatScore(memory.score)})</span
              >
              {#if memory.summary}
                <div class="text-default-600">{memory.summary}</div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/snippet}
</ExpandableMessageView>
