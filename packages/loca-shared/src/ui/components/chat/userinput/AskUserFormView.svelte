<script module lang="ts">
  // Working copy of in-flight askUser answers, indexed by question number.
  // Exported so a host scripting the form (the client composable, or the
  // website showcase with static state) can build the drafts snapshot.
  // `picks` holds option values (also the diff verdict, file paths, and the
  // image action); `freestyleActive` means the freeform text currently counts
  // as an answer; `rows` is the editable grid of a table question.
  export type DraftAnswer = {
    text: string;
    picks: string[];
    freestyleActive: boolean;
    rows: string[][];
  };
</script>

<script lang="ts">
  import type { AskUserQuestion } from "../../../../domain/session.ts";
  import DiffQuestion from "../messages/askuser/DiffQuestion.svelte";
  import FilesQuestion from "../messages/askuser/FilesQuestion.svelte";
  import ImageQuestion from "../messages/askuser/ImageQuestion.svelte";
  import TableQuestion from "../messages/askuser/TableQuestion.svelte";
  import ChoiceQuestion from "../messages/askuser/ChoiceQuestion.svelte";

  // Presentational askUser form: the question prompts and their inputs (option
  // tiles, freeform text, file lists, diff/image display, editable tables). The
  // commit actions (Submit, diff Reject/Accept, image actions) are NOT here:
  // they render as composer buttons, hoisted by the host. This View is fully
  // controlled - drafts and the mutation handlers arrive as props, so the
  // client feeds live state and the website feeds a static snapshot. It owns
  // only the keyboard-navigation and auto-focus DOM behavior over its own
  // markup; `onSubmit` is invoked on Enter when `canSubmit` is true.

  let {
    questions,
    drafts,
    togglePick = () => {},
    setText = () => {},
    onFreestyleFocus = () => {},
    onFreestyleBlur = () => {},
    setCell = () => {},
    addRow = () => {},
    removeRow = () => {},
    canSubmit = false,
    onSubmit = () => {},
    autoFocus = true,
  }: {
    questions: AskUserQuestion[];
    drafts: Record<number, DraftAnswer>;
    togglePick?: (idx: number, value: string, multi: boolean) => void;
    setText?: (idx: number, text: string) => void;
    onFreestyleFocus?: (idx: number) => void;
    onFreestyleBlur?: (idx: number) => void;
    setCell?: (idx: number, row: number, col: number, value: string) => void;
    addRow?: (idx: number, columns: number) => void;
    removeRow?: (idx: number, row: number) => void;
    /** Whether the form is answerable; gates Enter-to-submit. */
    canSubmit?: boolean;
    onSubmit?: () => void;
    /** Focus the first input when the form appears. Off for static hosts. */
    autoFocus?: boolean;
  } = $props();

  // Selected vs. unselected styling for option buttons and the freeform input.
  // While keyboard-navigating, the focused element's inversion preview is the
  // only highlight, so hover styling is suppressed.
  let keyboardNav = $state(false);
  let unselectedClasses = $derived(
    keyboardNav
      ? "bg-surface-inset text-default-800 focus:bg-surface-inset-strong"
      : "bg-surface-inset text-default-800 hover:bg-surface-inset-strong",
  );
  const selectedClasses = "bg-default-inverted-200 text-default-inverted-800";

  // A lone diff/image question hoists its verdict/action buttons to the composer
  // row; stacked questions keep them inline so the form stays answerable. The
  // host's action-button derivation mirrors this same single-question gate.
  let hoistActions = $derived(questions.length === 1);

  // Container-level keyboard navigation: arrows + Tab cycle focus across all
  // [data-tc-nav] elements, Enter activates buttons and submits when ready.
  let container: HTMLDivElement | undefined = $state();
  function onContainerKeydown(e: KeyboardEvent) {
    const target = e.target as HTMLElement | null;
    const root = e.currentTarget as HTMLElement;
    if (!target || !root) return;

    const cycle = (offset: number) => {
      const items = Array.from(root.querySelectorAll<HTMLElement>("[data-tc-nav]")).filter(
        (el) => !(el as HTMLButtonElement).disabled && el.offsetParent !== null,
      );
      if (items.length === 0) return;
      const idx = items.indexOf(target);
      const nextIdx =
        idx === -1
          ? offset > 0
            ? 0
            : items.length - 1
          : (idx + offset + items.length) % items.length;
      items[nextIdx].focus();
    };

    switch (e.key) {
      case "ArrowDown":
        keyboardNav = true;
        e.preventDefault();
        cycle(1);
        return;
      case "ArrowUp":
        keyboardNav = true;
        e.preventDefault();
        cycle(-1);
        return;
      case "Tab":
        keyboardNav = true;
        e.preventDefault();
        cycle(e.shiftKey ? -1 : 1);
        return;
      case "Enter":
        e.preventDefault();
        if (target.tagName === "BUTTON") {
          (target as HTMLButtonElement).click();
          if (target.hasAttribute("data-tc-aux")) return;
        }
        if (canSubmit) onSubmit();
        return;
    }
  }
  function onContainerPointerMove() {
    if (keyboardNav) keyboardNav = false;
    const active = document.activeElement as HTMLElement | null;
    if (active && active.tagName === "BUTTON" && container?.contains(active)) {
      active.blur();
    }
  }

  // Auto-focus the first nav element when the form appears so the user can
  // start typing or arrow-navigate without first reaching for the mouse.
  // preventScroll: this fires on mount, so if the form appears while its panel is
  // translated off-screen (mid navigation slide) a plain focus would scroll the
  // layer to reveal it and shift the whole UI (see UserInput / Settings search).
  let focused = $state(false);
  $effect(() => {
    if (!autoFocus || focused || !container) return;
    const first = container.querySelector<HTMLElement>("[data-tc-nav]");
    if (!first) return;
    first.focus({ preventScroll: true });
    focused = true;
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- `text-default-800` so the question prompts (and any other uncolored text)
     resolve through the host bubble's `--default-base` and read in its accent,
     matching the option tiles instead of standing out as neutral body text. -->
<div
  bind:this={container}
  class="flex flex-col gap-2 text-left min-w-0 text-default-800"
  onkeydown={onContainerKeydown}
  onpointermove={onContainerPointerMove}
>
  {#each questions as q, qi (qi)}
    <div class="flex flex-col gap-2">
      <div class="text-sm">{q.question}</div>
      {#if q.kind === "diff"}
        <DiffQuestion
          {q}
          {qi}
          draft={drafts[qi]}
          {selectedClasses}
          {unselectedClasses}
          {togglePick}
          displayOnly={hoistActions}
        />
      {:else if q.kind === "files"}
        <FilesQuestion
          {q}
          {qi}
          draft={drafts[qi]}
          {selectedClasses}
          {unselectedClasses}
          {togglePick}
        />
      {:else if q.kind === "image"}
        <ImageQuestion
          {q}
          {qi}
          draft={drafts[qi]}
          {selectedClasses}
          {unselectedClasses}
          {togglePick}
          displayOnly={hoistActions}
        />
      {:else if q.kind === "table"}
        <TableQuestion
          {q}
          {qi}
          draft={drafts[qi]}
          {unselectedClasses}
          {setCell}
          {addRow}
          {removeRow}
        />
      {:else}
        <ChoiceQuestion
          {q}
          {qi}
          draft={drafts[qi]}
          {selectedClasses}
          {unselectedClasses}
          {togglePick}
          {setText}
          {onFreestyleFocus}
          {onFreestyleBlur}
        />
      {/if}
    </div>
  {/each}
</div>
