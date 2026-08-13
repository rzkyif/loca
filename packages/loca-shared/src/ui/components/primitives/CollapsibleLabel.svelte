<script lang="ts">
  import { untrack, type Snippet } from "svelte";
  import { collapseLabel } from "../../animations.ts";
  import { useUiContext } from "../../context.ts";

  // A label that slides to width 0 / opacity 0 when collapsed (sidebar rows).
  // The width-scaled motion lives in the shared `collapseLabel`; the duration
  // policy comes from the UI context (settings-aware in the client, BASE_MS on
  // the website). `firstRun` skips the animation on initial paint.
  const ui = useUiContext();

  let {
    collapsed,
    class: className = "",
    children,
  }: {
    collapsed: boolean;
    class?: string;
    children: Snippet;
  } = $props();

  let el: HTMLElement | undefined = $state();
  let firstRun = true;

  // Inline style on first paint matches the initial collapsed state so the label
  // doesn't flash at full width before $effect takes over.
  const initialStyle = untrack(() => (collapsed ? "width: 0; opacity: 0;" : ""));

  $effect(() => {
    const c = collapsed;
    if (!el) return;
    collapseLabel(el, c, (baseMs) => (firstRun ? 0 : ui.animationDurationMs(baseMs)));
    firstRun = false;
  });
</script>

<span
  bind:this={el}
  class="inline-block overflow-hidden whitespace-nowrap {className}"
  style={initialStyle}
>
  {@render children()}
</span>
