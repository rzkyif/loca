<script lang="ts">
  import type { Accent } from "./object-types.ts";

  // A small status chip for cards and detail headers. Borrows the group-header
  // destination chip's FORM (Settings.svelte): label-height, uppercase, wide
  // tracking, sitting inline without growing the row. `accent` keeps the status
  // color (Enabled/Error/Current); neutral falls back to bg-surface-inset.
  let {
    label,
    icon,
    accent,
    title,
  }: { label: string; icon?: string; accent?: Accent; title?: string } = $props();

  // On card hover the card turns bg-surface-inset, so each chip shifts one step
  // lighter to stay distinct: neutral drops to the settings view background
  // (bg-surface), accents drop from their -200 to -100 shade.
  //
  // The hover bg targets the underlying vars directly rather than e.g.
  // `group-hover:bg-surface`: those tokens carry their own `dark:`, and an outer
  // group-hover composes to `group-hover:dark:` (selector `.group:hover .dark`),
  // which never matches (`.dark` is on <html>), so dark mode wrongly fell back to
  // the bright light value. Writing the variants explicitly keeps `dark:`
  // outermost (`.dark .group:hover ...`). Full strings so the UnoCSS extractor
  // sees every variant. (group-hover keys off the card's `group` class; the
  // detail header has no group, so chips stay at rest there.)
  const accentMap: Record<Accent, string> = {
    blue: "bg-accent-blue-200 text-accent-blue-700 group-hover:bg-[var(--accent-blue-100)] dark:group-hover:bg-[var(--accent-blue-d-100)]",
    green:
      "bg-accent-green-200 text-accent-green-700 group-hover:bg-[var(--accent-green-100)] dark:group-hover:bg-[var(--accent-green-d-100)]",
    red: "bg-accent-red-200 text-accent-red-700 group-hover:bg-[var(--accent-red-100)] dark:group-hover:bg-[var(--accent-red-d-100)]",
    yellow:
      "bg-accent-yellow-200 text-accent-yellow-700 group-hover:bg-[var(--accent-yellow-100)] dark:group-hover:bg-[var(--accent-yellow-d-100)]",
    purple:
      "bg-accent-purple-200 text-accent-purple-700 group-hover:bg-[var(--accent-purple-100)] dark:group-hover:bg-[var(--accent-purple-d-100)]",
  };

  const colorClass = $derived(
    accent
      ? accentMap[accent]
      : "bg-surface-inset text-default-700 group-hover:bg-[var(--default-50)] dark:group-hover:bg-[var(--default-d-50)]",
  );
</script>

<span
  class="shrink-0 inline-flex items-center gap-1 h-4 px-1.5 leading-none rounded-medium text-[10px] font-medium uppercase tracking-wider {colorClass}"
  {title}
>
  {#if icon}<i class="flex {icon} text-[10px]"></i>{/if}
  {label}
</span>
