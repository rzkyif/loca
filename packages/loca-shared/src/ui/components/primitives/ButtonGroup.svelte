<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";

  // A segmented container that visually joins a row/column of buttons on one
  // inset surface. Shared so client and website render grouped controls alike.
  type Size = "sm" | "md" | "lg";
  type Direction = "row" | "column";
  type Surface = "default" | "transparent";

  type Props = Omit<HTMLAttributes<HTMLDivElement>, "class" | "children"> & {
    size?: Size;
    direction?: Direction;
    surface?: Surface;
    class?: string;
    children: Snippet;
  };

  let {
    size = "md",
    direction = "row",
    surface = "default",
    class: extraClass = "",
    children,
    ...rest
  }: Props = $props();

  const sizeClass = $derived(
    {
      sm: "h-8 px-1",
      md: "p-1",
      lg: "px-2 py-1",
    }[size],
  );

  const dirClass = $derived(direction === "column" ? "flex-col" : "flex-row");
  const surfaceClass = $derived(surface === "transparent" ? "" : "bg-surface-inset");
</script>

<div
  {...rest}
  class="flex {dirClass} items-center justify-center {sizeClass} {surfaceClass} rounded-large {extraClass}"
>
  {@render children()}
</div>
