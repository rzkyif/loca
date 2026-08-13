<script lang="ts">
  import type { AskUserFilesQuestion } from "../../../../../domain/session.ts";
  import type { DraftAnswer } from "../../userinput/AskUserFormView.svelte";

  let {
    q,
    qi,
    draft,
    selectedClasses,
    unselectedClasses,
    togglePick,
  }: {
    q: AskUserFilesQuestion;
    qi: number;
    draft: DraftAnswer | undefined;
    selectedClasses: string;
    unselectedClasses: string;
    togglePick: (idx: number, value: string, multi: boolean) => void;
  } = $props();
</script>

<div class="flex flex-col gap-1">
  {#each q.entries as entry, ei (ei)}
    <button
      type="button"
      data-tc-nav
      class="text-xs px-2 py-1 min-h-8 rounded cursor-pointer text-left outline-none transition-colors duration-100 {draft?.picks.includes(
        entry.path,
      )
        ? selectedClasses
        : unselectedClasses}"
      title={entry.path}
      onclick={() => togglePick(qi, entry.path, !!q.multiselect)}
    >
      {entry.label ?? entry.path}
      {#if entry.description}
        <span class="opacity-60 ml-1">{entry.description}</span>
      {/if}
    </button>
  {/each}
</div>
