<script lang="ts">
  import Modal from "./Modal.svelte";
  import { useUiContext } from "../../context.ts";
  import { RIPPLE_MS } from "../../animations.ts";
  import { ripple } from "../../actions/ripple.ts";

  // A touch-first list of actions. On mobile it inherits Modal's bottom-sheet
  // presentation (full-width, pinned to the bottom, large tap targets); on the
  // desktop/website it renders as a small centered dialog. It replaces the
  // native context menu on the touch shell (long-press a message, a session,
  // ...). Single-source: it lives in shared primitives so the website gallery
  // can render it, and is simply only consumed by the client's mobile branch.

  export interface ActionSheetItem {
    label: string;
    /** UnoCSS icon class, e.g. "i-material-symbols-delete-outline-rounded". */
    icon?: string;
    onSelect: () => void;
    /** Renders the row in the destructive (red) treatment. */
    destructive?: boolean;
    disabled?: boolean;
  }

  let {
    open,
    onclose,
    title,
    items,
    ariaLabel,
  }: {
    open: boolean;
    onclose: () => void;
    title?: string;
    items: ActionSheetItem[];
    ariaLabel?: string;
  } = $props();

  const ui = useUiContext();
  const rippleDuration = $derived(ui.animationDurationMs(RIPPLE_MS));

  function choose(item: ActionSheetItem): void {
    if (item.disabled) return;
    // Run the action, THEN close. Order matters for a host that wires both
    // onSelect and onclose to one resolver: the selection resolves first and the
    // trailing close becomes a no-op, rather than the dismiss winning.
    item.onSelect();
    onclose();
  }
</script>

<Modal {open} {onclose} maxWidth="sm" ariaLabel={ariaLabel ?? title}>
  {#if title}
    <div class="px-2 pb-1 text-sm font-medium text-default-500">{title}</div>
  {/if}
  <div class="flex flex-col">
    {#each items as item (item.label)}
      <button
        type="button"
        class="flex items-center gap-3 rounded-medium px-2 py-3.5 text-left transition-interactive hov:cursor-pointer
          {item.destructive
          ? 'text-accent-red-700 hov:bg-accent-red-200/20'
          : 'text-default-800 hov:bg-surface-inset'}
          {item.disabled ? 'opacity-40 pointer-events-none' : ''}"
        onclick={() => choose(item)}
        use:ripple={{ disabled: item.disabled, durationMs: rippleDuration }}
      >
        {#if item.icon}
          <i class="{item.icon} shrink-0 text-xl"></i>
        {/if}
        <span class="text-base">{item.label}</span>
      </button>
    {/each}
  </div>
</Modal>
