<script lang="ts">
  import type { AskUserChoiceQuestion } from "../../../../../domain/session.ts";
  import type { DraftAnswer } from "../../userinput/AskUserFormView.svelte";

  let {
    q,
    qi,
    draft,
    selectedClasses,
    unselectedClasses,
    togglePick,
    setText,
    onFreestyleFocus,
    onFreestyleBlur,
  }: {
    q: AskUserChoiceQuestion;
    qi: number;
    draft: DraftAnswer | undefined;
    selectedClasses: string;
    unselectedClasses: string;
    togglePick: (idx: number, value: string, multi: boolean) => void;
    setText: (idx: number, text: string) => void;
    onFreestyleFocus: (idx: number) => void;
    onFreestyleBlur: (idx: number) => void;
  } = $props();
</script>

{#if q.options}
  <div class="flex flex-col gap-1">
    {#each q.options as opt, oi (oi)}
      <button
        type="button"
        data-tc-nav
        class="text-xs px-2 py-1 h-8 rounded cursor-pointer text-left outline-none transition-colors duration-100 {draft?.picks.includes(
          opt.value,
        )
          ? selectedClasses
          : unselectedClasses}"
        title={opt.description}
        onclick={() => togglePick(qi, opt.value, !!q.multiselect)}
      >
        {opt.label}
      </button>
    {/each}
  </div>
  {#if q.allowFreeformInput}
    <input
      type="text"
      data-tc-nav
      class="rounded block w-full h-8 px-2 outline-none -mt-1 text-xs transition-colors duration-100 {draft?.freestyleActive
        ? selectedClasses
        : unselectedClasses}"
      placeholder="Or type your own..."
      value={draft?.text ?? ""}
      oninput={(e) => setText(qi, (e.target as HTMLInputElement).value)}
      onfocus={() => onFreestyleFocus(qi)}
      onblur={() => onFreestyleBlur(qi)}
    />
  {/if}
{:else}
  <input
    type="text"
    data-tc-nav
    class="bg-surface-inset text-default-800 rounded block w-full h-8 px-2 outline-none text-xs transition-colors duration-100"
    value={draft?.text ?? ""}
    oninput={(e) => setText(qi, (e.target as HTMLInputElement).value)}
  />
{/if}
