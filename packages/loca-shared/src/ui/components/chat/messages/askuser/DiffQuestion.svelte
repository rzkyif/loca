<script lang="ts">
  import type { AskUserDiffQuestion } from "../../../../../domain/session.ts";
  import type { DraftAnswer } from "../../userinput/AskUserFormView.svelte";
  import DiffView from "../DiffView.svelte";

  // The diff verdict (Reject / Accept). For a lone question the actions are
  // hoisted to composer buttons (`displayOnly`); when stacked with other
  // questions they stay inline so the form remains answerable in place.
  let {
    q,
    qi = 0,
    draft = undefined,
    selectedClasses = "",
    unselectedClasses = "",
    togglePick = () => {},
    displayOnly = false,
  }: {
    q: AskUserDiffQuestion;
    qi?: number;
    draft?: DraftAnswer | undefined;
    selectedClasses?: string;
    unselectedClasses?: string;
    togglePick?: (idx: number, value: string, multi: boolean) => void;
    displayOnly?: boolean;
  } = $props();
</script>

{#if q.title}
  <div class="text-xs text-default-600">{q.title}</div>
{/if}
<div class="max-h-48 overflow-auto">
  <DiffView before={q.before} after={q.after} />
</div>
{#if !displayOnly}
  <!-- Bottom action row: full-width split, affirmative (Accept) on the right. -->
  <div class="flex gap-1">
    {#each [{ label: "Reject", value: "reject" }, { label: "Accept", value: "accept" }] as verdict (verdict.value)}
      <button
        type="button"
        data-tc-nav
        class="flex-1 text-xs px-3 h-8 rounded cursor-pointer outline-none transition-colors duration-100 {draft
          ?.picks[0] === verdict.value
          ? selectedClasses
          : unselectedClasses}"
        onclick={() => togglePick(qi, verdict.value, false)}
      >
        {verdict.label}
      </button>
    {/each}
  </div>
{/if}
