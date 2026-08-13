<script lang="ts">
  import type { AskUserImageQuestion } from "../../../../../domain/session.ts";
  import type { DraftAnswer } from "../../userinput/AskUserFormView.svelte";

  // The image actions. For a lone question the actions are hoisted to composer
  // buttons (`displayOnly`); when stacked with other questions they stay inline
  // so the form remains answerable in place.
  let {
    q,
    qi = 0,
    draft = undefined,
    selectedClasses = "",
    unselectedClasses = "",
    togglePick = () => {},
    displayOnly = false,
  }: {
    q: AskUserImageQuestion;
    qi?: number;
    draft?: DraftAnswer | undefined;
    selectedClasses?: string;
    unselectedClasses?: string;
    togglePick?: (idx: number, value: string, multi: boolean) => void;
    displayOnly?: boolean;
  } = $props();
</script>

<img
  src={`data:${q.mime};base64,${q.dataB64}`}
  alt={q.question}
  class="max-h-64 max-w-full self-start rounded-small"
/>
{#if !displayOnly}
  <!-- Bottom action row: full-width, split evenly across the actions. -->
  <div class="flex gap-1">
    {#each q.actions as action (action.value)}
      <button
        type="button"
        data-tc-nav
        class="flex-1 text-xs px-3 h-8 rounded cursor-pointer outline-none transition-colors duration-100 {draft
          ?.picks[0] === action.value
          ? selectedClasses
          : unselectedClasses}"
        onclick={() => togglePick(qi, action.value, false)}
      >
        {action.label}
      </button>
    {/each}
  </div>
{/if}
