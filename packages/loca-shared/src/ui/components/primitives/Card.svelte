<script lang="ts">
  import type { Snippet } from "svelte";

  type Variant = "default" | "raised";
  type Padding = "none" | "sm" | "md" | "lg";

  let {
    variant = "default",
    padding = "md",
    class: extraClass = "",
    children,
  }: {
    variant?: Variant;
    padding?: Padding;
    class?: string;
    children: Snippet;
  } = $props();

  const variantClass = $derived(variant === "raised" ? "bg-surface-inset" : "bg-surface");

  const padClass = $derived(
    { none: "", sm: "px-2 py-1", md: "px-3 py-2", lg: "px-5 py-4" }[padding],
  );
</script>

<div class="rounded-medium {variantClass} {padClass} {extraClass}">
  {@render children()}
</div>
