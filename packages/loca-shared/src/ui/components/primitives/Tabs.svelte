<script lang="ts">
  import { CSS_EASING, RIPPLE_MS } from "../../animations.ts";
  import { useUiContext } from "../../context.ts";
  import { ripple } from "../../actions/ripple.ts";
  import type { SettingTab } from "../../../domain/settings/types.ts";

  const ui = useUiContext();
  const rippleDuration = $derived(ui.animationDurationMs(RIPPLE_MS));

  // Segmented tab selector: a bg-surface-inset groove with every tab label
  // resting on it and a knob that slides to the active tab. The knob is an
  // aria-hidden overlay clipped to the active cell with clip-path: inset();
  // transitioning the clip rect slides it and repaints each label exactly where
  // the knob edge crosses. Labels are normal case; the knob is a subtle raised
  // shade (bg-surface-inset-strong, lighter than the groove in dark mode).
  let {
    tabs,
    active,
    onSelect,
    slideMs,
  }: {
    tabs: SettingTab[];
    active: string;
    onSelect: (id: string) => void;
    /** Knob transition duration. The caller passes the full tab-slide length so
     *  the knob travels for the whole animation; defaults to one base unit
     *  (settings-aware in the client via the UI context). */
    slideMs?: number;
  } = $props();

  const selectedIndex = $derived(
    Math.max(
      0,
      tabs.findIndex((t) => t.id === active),
    ),
  );

  const knobStyle = $derived.by(() => {
    const pad = "0.25rem"; // matches the groove's p-1
    const cell = `(100% - 2 * ${pad}) / ${tabs.length}`;
    const clip = `inset(${pad} calc(${pad} + ${cell} * ${
      tabs.length - 1 - selectedIndex
    }) ${pad} calc(${pad} + ${cell} * ${selectedIndex}) round var(--rounded-large))`;
    return `clip-path: ${clip}; transition: clip-path ${slideMs ?? ui.animationDurationMs()}ms ${CSS_EASING};`;
  });

  // min-w-0 keeps the cells exactly equal-width even if a label overflows, since
  // the knob clip assumes uniform Nths.
  const cellClass = "flex-1 min-w-0 flex items-center justify-center px-2 text-sm";
</script>

<div class="groove relative flex w-full h-10 rounded-large bg-surface-inset p-1" role="tablist">
  {#each tabs as tab (tab.id)}
    <button
      type="button"
      role="tab"
      aria-selected={active === tab.id}
      class="{cellClass} text-default-600 transition-interactive hov:text-default-800 hov:cursor-pointer"
      onclick={() => onSelect(tab.id)}
      use:ripple={{ durationMs: rippleDuration }}
    >
      {tab.label}
    </button>
  {/each}
  <span
    aria-hidden="true"
    class="absolute inset-0 flex p-1 bg-surface-inset-strong text-default-900 pointer-events-none"
    style={knobStyle}
  >
    {#each tabs as tab (tab.id)}
      <span class={cellClass}>{tab.label}</span>
    {/each}
  </span>
</div>

<style>
  /* The active tab button sits under the opaque knob, where its focus ring
     would be painted over; resurface keyboard focus on the groove itself
     (mirrors Toggle). */
  .groove:has(:focus-visible) {
    outline: var(--focus-ring);
    outline-offset: var(--focus-ring-offset);
  }
</style>
