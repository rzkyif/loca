<script lang="ts">
  import type { Snippet } from "svelte";
  import HelpText from "./HelpText.svelte";
  import { useUiContext } from "../../context.ts";
  import { RIPPLE_MS } from "../../animations.ts";
  import { ripple } from "../../actions/ripple.ts";

  // A selectable card/row (preset pickers, tool-call options, model cards).
  // Shared so client and website render option cards identically. Hover bits use
  // `hov:` for demo-cursor parity.
  type Size = "sm" | "md";
  type SelectedStyle = "invert" | "accent";
  type Accent = "blue" | "green" | "red" | "yellow" | "purple";

  let {
    selected,
    size = "md",
    selectedStyle = "invert",
    accent = "blue",
    icon,
    title,
    description,
    badges,
    trailing,
    onclick,
    ariaLabel,
    htmlTitle,
    class: extraClass = "",
    children,
  }: {
    selected: boolean;
    size?: Size;
    selectedStyle?: SelectedStyle;
    accent?: Accent;
    icon?: string;
    title?: string;
    description?: string;
    badges?: Snippet;
    /** Right-aligned content in the title row (e.g. a "Recommended" chip). */
    trailing?: Snippet;
    onclick: () => void;
    ariaLabel?: string;
    htmlTitle?: string;
    class?: string;
    children?: Snippet;
  } = $props();

  const sizeClass = $derived(
    size === "sm" ? "text-xs px-2 py-1 h-8 rounded gap-1.5" : "p-3 rounded-large gap-1.5",
  );

  // Full strings so the UnoCSS extractor sees every accent.
  const accentSelectedMap: Record<Accent, string> = {
    blue: "border-accent-blue-300 bg-accent-blue-100",
    green: "border-accent-green-300 bg-accent-green-100",
    red: "border-accent-red-300 bg-accent-red-100",
    yellow: "border-accent-yellow-300 bg-accent-yellow-100",
    purple: "border-accent-purple-300 bg-accent-purple-100",
  };

  // Unselected cards follow the shared interaction standard (rest fill darkens
  // one step on hover; the press splash is the shared `use:ripple` action); the
  // selected treatment stays distinct.
  const stateClass = $derived(
    selectedStyle === "accent"
      ? selected
        ? `border-2 ${accentSelectedMap[accent]} text-default-800`
        : "border-2 border-transparent bg-surface-inset hov:bg-surface-inset-strong text-default-800"
      : selected
        ? "bg-default-inverted-300 text-default-inverted-900"
        : "bg-surface-inset text-default-800 hov:bg-surface-inset-strong",
  );

  const ui = useUiContext();
  const rippleDuration = $derived(ui.animationDurationMs(RIPPLE_MS));

  const descriptionClass = $derived(
    selectedStyle === "invert" && selected ? "text-default-inverted-600" : "text-default-500",
  );
  const badgesTextClass = $derived(
    selectedStyle === "invert" && selected ? "text-default-inverted-600" : "text-default-600",
  );
</script>

<button
  type="button"
  class="cursor-pointer text-left flex flex-col outline-none transition-interactive {sizeClass} {stateClass} {extraClass}"
  title={htmlTitle}
  aria-label={ariaLabel}
  {onclick}
  use:ripple={{ durationMs: rippleDuration }}
>
  {#if children}
    {@render children()}
  {:else if size === "sm"}
    {title}
  {:else}
    <!-- min-h keeps the title row a constant height whether or not a trailing
         chip is present, so descriptions line up across sibling cards. -->
    <div class="flex items-center gap-1.5 min-h-6">
      {#if icon}
        <i class="{icon} text-lg"></i>
      {/if}
      {#if title}
        <span class="text-base font-semibold leading-tight">{title}</span>
      {/if}
      {#if trailing}
        <span class="ml-auto flex items-center">{@render trailing()}</span>
      {/if}
    </div>
    {#if badges}
      <div class="flex flex-wrap gap-x-3 gap-y-1 text-xs {badgesTextClass}">
        {@render badges()}
      </div>
    {/if}
    {#if description}
      <HelpText text={description} variant="compact" class={descriptionClass} />
    {/if}
  {/if}
</button>
