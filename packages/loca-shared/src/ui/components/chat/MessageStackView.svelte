<script lang="ts">
  import type { Snippet } from "svelte";

  // Presentational chrome for a horizontal substack: a horizontally-scrollable
  // row of collapsed message bubbles with edge-fade overlays that hint at
  // hidden content past either scroll edge. The client owns the live messages,
  // the scroll/measurement logic, sticky-scroll pinning, neighbor computation,
  // and the expansion click handling; it feeds this View the per-side fade
  // widths and the bubbles themselves (via the `bubble` snippet, rendered once
  // per index inside this View's shadow-room clipping wrapper), and binds the
  // scroll `wrapper` so it can keep observing/measuring it.
  let {
    count,
    alignment,
    fadeLeft = "0px",
    fadeRight = "0px",
    onClickCapture,
    onWheel,
    onScroll,
    wrapper = $bindable(),
    bubble,
  }: {
    /** Number of bubbles in the row; the `bubble` snippet renders once per index. */
    count: number;
    /** "right" lays the row out reversed (flex-row-reverse), matching the
     *  window-alignment setting / mobile user-group hug. */
    alignment: "left" | "center" | "right";
    /** Width of the left edge-fade overlay ("1rem" when content is hidden past
     *  the left edge, "0px" otherwise). */
    fadeLeft?: string;
    /** Width of the right edge-fade overlay. */
    fadeRight?: string;
    onClickCapture?: (e: MouseEvent) => void;
    onWheel?: (e: WheelEvent) => void;
    onScroll?: (e: Event) => void;
    /** The scroll row, bound back to the client for measurement/observation. */
    wrapper?: HTMLDivElement;
    /** Renders the bubble at index `i` (the client supplies the live bubble). */
    bubble: Snippet<[number]>;
  } = $props();

  const noop = (): void => {};
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<!-- Outer host: relative positioning anchors the edge-fade overlays to the
     wrapper's viewport, so they stay pinned to the visible edges regardless
     of scroll position. No stacking context of its own: the bubbles' z-0
     shadow layers and z-10 bodies must resolve against the rest of the row's
     stacking context so no shadow ever covers another bubble; the overlays'
     `z-30` still ranks above the Bubble's internal `z-10` content / `z-20`
     progress layers, letting the fade cover the bubble text and not just the
     background. -->
<div class="relative w-fit max-w-[calc(100vw-5rem)]">
  <div
    bind:this={wrapper}
    role="presentation"
    class="stack-shadow-room flex gap-1 overflow-x-auto no-scrollbar pointer-events-auto cursor-pointer"
    class:flex-row-reverse={alignment === "right"}
    onclickcapture={onClickCapture ?? noop}
    onwheel={onWheel ?? noop}
    onscroll={onScroll ?? noop}
  >
    {#each Array(count) as _, i (i)}
      <!-- `max-h-8 overflow-hidden` clips any in-flight expanded body to the
           small-bubble height; `flex-shrink-0` keeps the bubble at its
           natural width so the row overflows into the scroll container.
           `box-content` keeps the max-height measuring the bubble itself
           while the shadow-room padding stays outside it. -->
      <div class="stack-shadow-room box-content max-h-8 overflow-hidden flex-shrink-0">
        {@render bubble(i)}
      </div>
    {/each}
  </div>
  <!-- Edge fades: solid `bg-surface` at the scrollable edge fading to
       transparent towards the row, hinting that more bubbles exist past
       the viewport without using a hard cutoff. Width collapses to 0 when
       there's nothing hidden on that side. Anchored to the row's actual
       clip edge, which sits one shadow-room distance outside this host
       (the scroll row's padding/negative-margin trick below), and sized to
       the bubble strip (h-8), not the shadow gutter above/below it.
       Vertically the row's negative top margin collapses through this host
       (parent-child margin collapse applies vertically only), so the bubble
       strip starts one shadow distance below host top; the fades offset by
       the same amount. -->
  <div
    class="absolute h-8 z-30 bg-surface pointer-events-none stack-fade-left"
    style:top="var(--bubble-shadow-distance)"
    style:left="calc(-1 * var(--bubble-shadow-distance))"
    style:width={fadeLeft}
  ></div>
  <div
    class="absolute h-8 z-30 bg-surface pointer-events-none stack-fade-right"
    style:top="var(--bubble-shadow-distance)"
    style:right="calc(-1 * var(--bubble-shadow-distance))"
    style:width={fadeRight}
  ></div>
</div>

<style>
  /* Both the scroll row and each clipping item wrapper swallow the bubbles'
     drop shadow (overflow-x-auto / overflow-hidden clip at the border box).
     Pad each by the shadow distance so the shadow has room to paint, and
     hand the space back with a negative margin so the laid-out size (and the
     visual gap between bubbles) is unchanged. */
  .stack-shadow-room {
    padding: var(--bubble-shadow-distance);
    margin: calc(-1 * var(--bubble-shadow-distance));
  }

  .stack-fade-left {
    mask-image: linear-gradient(to right, black, transparent);
    -webkit-mask-image: linear-gradient(to right, black, transparent);
  }
  .stack-fade-right {
    mask-image: linear-gradient(to left, black, transparent);
    -webkit-mask-image: linear-gradient(to left, black, transparent);
  }
</style>
