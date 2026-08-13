<script lang="ts" module>
  // Anything with a name + @trigger can be offered: snippets and memories
  // share this dropdown.
  export type TriggerOption = { id: string; name: string; trigger: string };
</script>

<script lang="ts">
  import ListItem from "../primitives/ListItem.svelte";
  import { useUiContext } from "../../context.ts";

  // Presentational autocomplete dropdown anchored at a fixed position, listing
  // trigger options. All behavior (which options, selection index, what a
  // selection does) is owned by the caller and supplied via props.
  let {
    options,
    selectedIndex,
    anchor,
    onSelect,
  }: {
    options: TriggerOption[];
    selectedIndex: number;
    anchor: { top: number; left: number };
    onSelect: (option: TriggerOption) => void;
  } = $props();

  const ui = useUiContext();
  // On touch the caret-anchored popover is too small to tap and the soft
  // keyboard hides it. Present the suggestions as a full-width sheet that opens
  // upward from just above the caret line (`anchor.top`), so it sits over the
  // composer and clear of the keyboard. The single-source rule keeps both forms
  // in this one shared View; the desktop popover is untouched.
  const sheet = $derived(ui.platform === "mobile");
</script>

{#if options.length > 0}
  {#if sheet}
    <div
      class="loca-scroll fixed left-0 right-0 z-50 bg-surface rounded-t-large shadow-xl border-t border-x border-surface overflow-y-auto max-h-[40dvh] overscroll-contain pointer-events-auto"
      style="bottom: calc(100dvh - {anchor.top}px);"
      role="listbox"
    >
      {@render rows("py-3.5")}
    </div>
  {:else}
    <div
      class="loca-scroll fixed z-50 bg-surface rounded-large shadow-xl border border-surface overflow-hidden min-w-48 max-w-80 max-h-72 overflow-y-auto pointer-events-auto"
      style="top: {anchor.top}px; left: {anchor.left}px;"
      role="listbox"
    >
      {@render rows("py-2")}
    </div>
  {/if}
{/if}

{#snippet rows(rowPad: string)}
  {#each options as option, i (option.id)}
    <!-- onmousedown (not onclick) so the textarea keeps focus through the
         selection; preventDefault avoids the blur. (Touch synthesizes a
         mousedown before blur, so this fires on tap too.) -->
    <ListItem
      direction="row"
      selected={i === selectedIndex}
      role="option"
      ariaSelected={i === selectedIndex}
      class="rounded-none px-4 {rowPad}"
      onmousedown={(e) => {
        e.preventDefault();
        onSelect(option);
      }}
    >
      <span class="truncate flex-1 text-sm text-default-800">
        {option.name || option.trigger}
      </span>
      <span class="text-xs font-mono text-default-600 shrink-0">
        {option.trigger}
      </span>
    </ListItem>
  {/each}
{/snippet}
