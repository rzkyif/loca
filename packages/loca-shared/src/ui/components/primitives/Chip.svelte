<script lang="ts">
  import type { Snippet } from "svelte";
  import { useUiContext } from "../../context.ts";
  import { RIPPLE_MS } from "../../animations.ts";
  import { ripple } from "../../actions/ripple.ts";

  // A compact pill: optional icon + label (or custom children), in a neutral or
  // accent surface, optionally clickable. Shared so the client and website paint
  // chips identically.
  type Size = "xs" | "sm" | "md";
  type Variant = "default" | "subtle" | "accent";
  type Accent = "blue" | "green" | "red" | "yellow" | "purple";

  let {
    label,
    icon,
    size = "md",
    variant = "default",
    accent = "blue",
    truncate = false,
    labelMaxWidth,
    title,
    onclick,
    class: extraClass = "",
    children,
  }: {
    label?: string;
    icon?: string;
    size?: Size;
    variant?: Variant;
    accent?: Accent;
    truncate?: boolean;
    labelMaxWidth?: string;
    title?: string;
    onclick?: (e: MouseEvent) => void;
    class?: string;
    children?: Snippet;
  } = $props();

  const sizeClass = $derived(
    {
      xs: "px-1.5 py-0.5 text-xs gap-1 rounded-medium",
      sm: "px-2 py-0.5 text-xs gap-1.5 rounded-large h-6",
      md: "px-2.5 h-8 text-sm gap-1.5 rounded-large",
    }[size],
  );

  const iconSize = $derived(size === "md" ? "text-base" : "text-sm");

  // Full strings so the UnoCSS extractor sees every variant.
  const accentMap: Record<Accent, string> = {
    blue: "bg-accent-blue-200 text-accent-blue-700",
    green: "bg-accent-green-200 text-accent-green-700",
    red: "bg-accent-red-200 text-accent-red-700",
    yellow: "bg-accent-yellow-200 text-accent-yellow-700",
    purple: "bg-accent-purple-200 text-accent-purple-700",
  };

  const colorClass = $derived(
    variant === "subtle"
      ? "bg-surface-inset-strong text-default-600"
      : variant === "accent"
        ? accentMap[accent]
        : "bg-surface-inset text-default-600",
  );

  // Clickable chips follow the shared interaction standard: rest fill darkens
  // one shade step on hover; the press splash is the shared `use:ripple` action.
  // Full strings for the extractor.
  const accentHoverMap: Record<Accent, string> = {
    blue: "hov:bg-accent-blue-300",
    green: "hov:bg-accent-green-300",
    red: "hov:bg-accent-red-300",
    yellow: "hov:bg-accent-yellow-300",
    purple: "hov:bg-accent-purple-300",
  };

  const hoverClass = $derived(
    variant === "subtle"
      ? "hov:bg-default-400"
      : variant === "accent"
        ? accentHoverMap[accent]
        : "hov:bg-default-300",
  );

  const interactive = $derived(!!onclick);
  const ui = useUiContext();
  const rippleDuration = $derived(ui.animationDurationMs(RIPPLE_MS));
  const baseClass = $derived(
    `inline-flex items-center shrink-0 ${sizeClass} ${colorClass} ${
      interactive ? `hov:cursor-pointer transition-interactive ${hoverClass}` : ""
    } ${extraClass}`,
  );
  const labelClass = $derived(truncate ? "truncate min-w-0" : "");
</script>

{#snippet body()}
  {#if icon}<i class="flex {icon} {iconSize} shrink-0"></i>{/if}
  {#if children}
    {@render children()}
  {:else if label}
    <span class={labelClass} style:max-width={labelMaxWidth}>{label}</span>
  {/if}
{/snippet}

{#if interactive}
  <button
    class={baseClass}
    type="button"
    {title}
    {onclick}
    use:ripple={{ durationMs: rippleDuration }}
  >
    {@render body()}
  </button>
{:else}
  <span class={baseClass} {title}>
    {@render body()}
  </span>
{/if}
