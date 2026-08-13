<script lang="ts">
  import type { Snippet } from "svelte";
  import { useUiContext } from "../../context.ts";
  import { RIPPLE_MS } from "../../animations.ts";
  import { ripple } from "../../actions/ripple.ts";

  type Variant = "primary" | "secondary" | "destructive" | "ghost";
  type Size = "sm" | "md";

  let {
    variant = "secondary",
    size = "md",
    icon,
    disabled = false,
    loading = false,
    title,
    ariaLabel,
    type = "button",
    onclick,
    class: extraClass = "",
    children,
  }: {
    variant?: Variant;
    size?: Size;
    icon?: string;
    disabled?: boolean;
    /** Shows a spinner in place of `icon` and disables the button. */
    loading?: boolean;
    title?: string;
    ariaLabel?: string;
    type?: "button" | "submit";
    onclick?: (e: MouseEvent) => void;
    class?: string;
    children?: Snippet;
  } = $props();

  const displayIcon = $derived(
    loading ? "i-material-symbols-progress-activity animate-spin" : icon,
  );
  const iconSizeClass = $derived(size === "sm" ? "text-sm" : "text-base");

  // Hover darkens the rest background one shade step (the shared interaction
  // standard); the press splash is the shared `use:ripple` action, not a color
  // shift. Ghost has no rest fill, so it materializes the inset surface on hover.
  const variantClass = $derived(
    {
      primary: "bg-default-inverted-300 text-default-inverted-900 hov:bg-default-inverted-400",
      secondary: "bg-surface-inset text-default-800 hov:bg-default-300",
      destructive: "bg-accent-red-200 text-accent-red-900 hov:bg-accent-red-300",
      ghost: "bg-transparent text-default-800 hov:bg-surface-inset",
    }[variant],
  );

  const ui = useUiContext();
  const rippleDuration = $derived(ui.animationDurationMs(RIPPLE_MS));

  const sizeClass = $derived(
    size === "sm" ? "px-2 py-1 text-xs gap-1" : "px-3 py-1.5 text-sm gap-1.5",
  );
</script>

<button
  {type}
  disabled={disabled || loading}
  {title}
  aria-label={ariaLabel}
  {onclick}
  class="inline-flex items-center justify-center rounded-medium {sizeClass} {variantClass} hov:cursor-pointer transition-interactive disabled:opacity-50 disabled:pointer-events-none {extraClass}"
  use:ripple={{ disabled: disabled || loading, durationMs: rippleDuration }}
>
  {#if displayIcon}
    <!-- Icon pinned to the leading edge; the label centers across the whole
         button via flex-1, and an invisible mirror icon on the trailing edge
         balances the width so the centering is true to the button, not to the
         space left over after the icon. -->
    <i class="flex {displayIcon} {iconSizeClass} shrink-0"></i>
    <span class="flex-1 text-center">{@render children?.()}</span>
    <i class="flex {displayIcon} {iconSizeClass} shrink-0 invisible" aria-hidden="true"></i>
  {:else}
    {@render children?.()}
  {/if}
</button>
