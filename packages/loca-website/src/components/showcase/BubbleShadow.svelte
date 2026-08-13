<script lang="ts">
  // Website-only chrome: renders the same drop shadow + frosted blur halo the
  // shared Bubble paints, as sibling layers behind an arbitrary card that is NOT
  // a chat bubble (the extension editor, the caption pill). The `.bubble-shadow`
  // and `.bubble-halo` classes and their geometry live in the shared base.css;
  // the ring count is read from the same UI context that drives real bubbles, so
  // these cards track the appearance settings the same way. Drop this inside a
  // `relative` wrapper whose body sibling carries `relative z-10 bubble-promote`.
  import { useUiContext } from "@loca/shared/ui/context";

  const ui = useUiContext();
  const ringCount = $derived(ui.bubbleBlurEnabled ? (ui.bubbleBlurRings ?? 1) : 0);
</script>

<div class="bubble-shadow absolute inset-0 z-0" aria-hidden="true"></div>
{#each Array(ringCount) as _, i (i)}
  <div
    class="bubble-halo"
    style="--ring-index: {i}; --ring-count: {ringCount}"
    aria-hidden="true"
  ></div>
{/each}
