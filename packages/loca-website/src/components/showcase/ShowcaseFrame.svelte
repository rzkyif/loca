<script lang="ts">
  import { onMount, type Snippet } from "svelte";
  import { APP_H, APP_SHADOW, APP_W } from "../../lib/showcase";

  // Scale-to-fit showcase frame. Renders a stage at its fixed design size
  // (DESIGN_W wide, `boxH` tall, both including the shadow halo margin) and
  // CSS-scales the whole thing down until it fits this element's flex-grown bounds
  // on BOTH axes, never up (cap 1), so the stage never clips or shows a scrollbar.
  // The natural width is fixed; the natural height defaults to the tallest surface
  // and a chat-style stage refines it once via the `setHeight` callback (passed to
  // the children snippet) with its measured content height.
  const DESIGN_W = APP_W + 2 * APP_SHADOW;

  let { children }: { children: Snippet<[(contentH: number) => void]> } = $props();

  let boxH = $state(APP_H + 2 * APP_SHADOW);
  let outer: HTMLElement | undefined = $state();
  let boundsW = $state(0);
  let boundsH = $state(0);

  const scale = $derived(
    boundsW > 0 && boundsH > 0 ? Math.min(boundsW / DESIGN_W, boundsH / boxH, 1) : 1,
  );

  function setHeight(contentH: number): void {
    boxH = contentH + 2 * APP_SHADOW;
  }

  onMount(() => {
    if (!outer) return;
    // Read the new bounds on the next frame, not synchronously inside the
    // observer callback: writing the scale (which retransforms the frame) during
    // dispatch is what triggers the benign "ResizeObserver loop" console error.
    const ro = new ResizeObserver(() => {
      requestAnimationFrame(() => {
        if (!outer) return;
        boundsW = outer.clientWidth;
        boundsH = outer.clientHeight;
      });
    });
    ro.observe(outer);
    return () => ro.disconnect();
  });
</script>

<div bind:this={outer} class="relative w-full flex-1 min-h-0">
  <!-- Patterned focus grid behind the stage, capped to the content width and
       centered (see `.focus-grid-stage` in site.css), giving the stage's blurred
       borders texture to sample. -->
  <div class="focus-grid-stage" aria-hidden="true"></div>

  <!-- The `.demo-frame` class is the shared frame styling (the 1:1 bubble-width
       re-pin in site.css), the same family the manual's demo frames use. -->
  <div
    class="demo-frame absolute left-1/2 top-1/2 origin-center"
    style="width: {DESIGN_W}px; height: {boxH}px; transform: translate(-50%, -50%) scale({scale})"
  >
    {@render children(setHeight)}
  </div>
</div>
