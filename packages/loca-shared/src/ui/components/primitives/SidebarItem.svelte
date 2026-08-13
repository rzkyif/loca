<script lang="ts">
  import type { Snippet } from "svelte";
  import CollapsibleLabel from "./CollapsibleLabel.svelte";
  import { useUiContext } from "../../context.ts";
  import { RIPPLE_MS } from "../../animations.ts";
  import { ripple } from "../../actions/ripple.ts";

  // A sidebar row: leading icon + collapsible label, with selected / hover /
  // ping states. Shared so the client settings sidebar and the website showcase
  // render rows identically. Interactive states use the `hov:` variant so a
  // scripted demo cursor (setting `data-hover`) resolves the same styles as a
  // real pointer.
  let {
    icon,
    label,
    collapsed,
    selected = false,
    ping = false,
    pingTone = "default",
    badge,
    onclick,
    title,
    ariaLabel,
    ariaPressed,
    disabled = false,
    class: extraClass = "",
  }: {
    icon: string;
    label?: string;
    collapsed: boolean;
    selected?: boolean;
    /** Pulse the text color to draw the eye (attention "ping"). */
    ping?: boolean;
    /** Ping palette: "default" pulses grey brightness, "accent" pulses yellow. */
    pingTone?: "default" | "accent";
    /** Optional badge content at the icon's top-right. */
    badge?: Snippet;
    onclick: () => void;
    title?: string;
    ariaLabel?: string;
    ariaPressed?: boolean;
    disabled?: boolean;
    class?: string;
  } = $props();

  // With a label the trailing pad collapses with it so the row reduces to a
  // centred icon; icon-only rows stay symmetric rectangles.
  const padRight = $derived(label ? (collapsed ? "pr-0" : "pr-2.5") : "pr-1.5");

  const ui = useUiContext();
  const mobile = $derived(ui.platform === "mobile");

  // Mobile renders each row as a tappable card (taller, padded, rounded-large,
  // resting on an inset fill) like a native iOS/Android settings row; desktop
  // keeps the compact flush sidebar row that only fills in on hover.
  const layoutClass = $derived(
    mobile
      ? "min-h-12 px-3 gap-3 rounded-large text-lg"
      : `h-8 pl-1.5 ${padRight} gap-1.5 rounded-medium`,
  );

  // The unselected resting tone brightens on hover; a coarse pointer (touch)
  // can't hover, so rest it at the brighter shade instead of leaving it dim.
  const restingText = $derived(
    ui.pointer === "coarse" ? "text-default-700" : "text-default-500 hov:text-default-700",
  );

  // Text tone by state (selected / ping / resting).
  const textTone = $derived(
    selected
      ? "text-default-900"
      : pingTone === "accent"
        ? ping
          ? "text-accent-yellow-700"
          : "text-accent-yellow-500"
        : ping
          ? "text-default-700"
          : restingText,
  );

  // Background: selected always fills; mobile rows always rest on a card (a shade
  // deeper on hover/press), desktop materializes the fill only on hover.
  const bgClass = $derived(
    selected
      ? "bg-surface-inset"
      : mobile
        ? "bg-surface-inset hov:bg-surface-inset-strong"
        : "hov:bg-surface-inset",
  );

  const rippleDuration = $derived(ui.animationDurationMs(RIPPLE_MS));
</script>

<!-- color/background-color use the shared 120ms interaction feedback (kept in
     sync with INTERACTIVE_MS / `transition-interactive`); padding keeps its own
     200ms because it animates the sidebar collapse, not button feedback, and so
     cannot share the single-property `transition-interactive` shortcut. -->
<button
  class="hov:cursor-pointer flex items-center {layoutClass} [transition:color_120ms,background-color_120ms,padding_200ms] disabled:opacity-50 disabled:pointer-events-none {textTone} {bgClass} {extraClass}"
  {title}
  aria-label={ariaLabel ?? label}
  aria-pressed={ariaPressed}
  {disabled}
  {onclick}
  use:ripple={{ disabled, durationMs: rippleDuration }}
>
  <span class="relative flex shrink-0">
    <i class="flex {mobile ? 'text-2xl' : 'text-xl'} shrink-0 {icon}"></i>
    {#if badge}
      {@render badge()}
    {/if}
  </span>
  {#if label}
    <CollapsibleLabel {collapsed} class="text-base text-left">
      {label}
    </CollapsibleLabel>
  {/if}
</button>
