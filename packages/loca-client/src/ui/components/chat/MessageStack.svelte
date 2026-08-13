<script lang="ts">
  import type { Snippet } from "svelte";
  import { tick } from "svelte";
  import type { Alignment, Message } from "$lib/util/types";
  import { expansionState } from "$stores/expansion.svelte";
  import MessageStackView from "@loca/shared/ui/components/chat/MessageStackView.svelte";

  let {
    messages,
    baseIdx,
    alignment,
    item,
  }: {
    messages: Message[];
    /** Index of `messages[0]` within the parent group's full message list.
     *  Forwarded into the snippet so per-bubble fallback keys (used in
     *  +page.svelte's `msgKey`) stay stable across substack splits. */
    baseIdx: number;
    alignment: Alignment;
    item: Snippet<
      [
        {
          msg: Message;
          idx: number;
          neighborLeft: boolean;
          neighborRight: boolean;
        },
      ]
    >;
  } = $props();

  let wrapper: HTMLDivElement | undefined = $state();

  // Sticky-scroll-to-end: the substack pins itself to the latest bubble
  // by default, and only stops once the user explicitly scrolls away.
  // Re-pins as soon as they scroll back to the end.
  //
  // Why we hand-roll this instead of `lastElementChild.scrollIntoView`:
  // scrollIntoView walks every scrollable ancestor, so during streaming
  // (when this is fired from the ResizeObserver below) it would also pull
  // the outer chat `<main>` back down whenever the user scrolled up,
  // overriding their vertical scroll position. Adjusting `wrapper.scrollLeft`
  // from rect math keeps the side-effect scoped to this row and works
  // under flex-row-reverse where WebKit reports scrollLeft in [-max, 0].
  let userScrolled = $state(false);

  function isAtEnd(): boolean {
    if (!wrapper || !wrapper.lastElementChild) return true;
    const wrapperRect = wrapper.getBoundingClientRect();
    const lastRect = wrapper.lastElementChild.getBoundingClientRect();
    // flex-row-reverse (right alignment): lastElementChild visually sits at
    // the wrapper's left. "end" means its left edge fits inside.
    if (alignment === "right") {
      return lastRect.left >= wrapperRect.left - 4;
    }
    // LTR (left/center alignment): lastElementChild is at the right edge.
    return lastRect.right <= wrapperRect.right + 4;
  }

  function pinToEnd() {
    if (!wrapper) return;
    if (userScrolled) return;
    if (!wrapper.lastElementChild) return;
    const wrapperRect = wrapper.getBoundingClientRect();
    const lastRect = wrapper.lastElementChild.getBoundingClientRect();
    const delta =
      alignment === "right" ? lastRect.left - wrapperRect.left : lastRect.right - wrapperRect.right;
    if (delta !== 0) wrapper.scrollLeft += delta;
  }

  // Edge-fade widths driving the colored overlays at the scrollable edges.
  // Non-zero on a side means there's hidden content past that edge, so the
  // overlay on that side is shown (default-100 fading to transparent into
  // the row). 0 collapses the overlay so the edge stays solid.
  let fadeLeft = $state("0px");
  let fadeRight = $state("0px");
  function updateFade() {
    if (!wrapper) return;
    const first = wrapper.firstElementChild;
    const last = wrapper.lastElementChild;
    if (!first || !last) {
      fadeLeft = "0px";
      fadeRight = "0px";
      return;
    }
    // Drive fades from element rects, not scrollLeft. Under flex-row-reverse
    // (right alignment) Chromium/WebKit report scrollLeft in a [-max, 0]
    // range, so any scrollLeft-derived math leaves one of the fades stuck.
    const wrapperRect = wrapper.getBoundingClientRect();
    const leftChild = alignment === "right" ? last : first;
    const rightChild = alignment === "right" ? first : last;
    const hiddenLeft = wrapperRect.left - leftChild.getBoundingClientRect().left;
    const hiddenRight = rightChild.getBoundingClientRect().right - wrapperRect.right;
    fadeLeft = hiddenLeft > 4 ? "1rem" : "0px";
    fadeRight = hiddenRight > 4 ? "1rem" : "0px";
  }

  // Translate vertical wheel events into horizontal scroll on this row, so
  // a plain mouse wheel scrolls along the stack without requiring shift.
  // Shift-wheel and trackpad horizontal gestures already produce non-zero
  // `deltaX` in the browser, so we prefer that when present and fall back
  // to `deltaY` otherwise. At the boundaries we let the event pass through
  // so the page can still scroll vertically once the stack hits its edge.
  function onWheel(e: WheelEvent) {
    if (!wrapper) return;
    if (wrapper.scrollWidth <= wrapper.clientWidth) return;
    const first = wrapper.firstElementChild;
    const last = wrapper.lastElementChild;
    if (!first || !last) return;
    // Trackpad gestures often emit a small non-zero deltaX alongside a much
    // larger deltaY; picking by magnitude avoids letting that jitter swallow
    // a vertical wheel translation.
    const delta = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
    if (delta === 0) return;
    // Detect visual edges via element rects (not scrollLeft), since under
    // flex-row-reverse the scrollLeft range flips negative and the usual
    // `>= max` check would never fire, leaving the wheel handler eating
    // events past the actual edge instead of letting the page scroll.
    const wrapperRect = wrapper.getBoundingClientRect();
    const leftChild = alignment === "right" ? last : first;
    const rightChild = alignment === "right" ? first : last;
    const atLeft = leftChild.getBoundingClientRect().left >= wrapperRect.left - 4;
    const atRight = rightChild.getBoundingClientRect().right <= wrapperRect.right + 4;
    if ((delta < 0 && atLeft) || (delta > 0 && atRight)) return;
    e.preventDefault();
    wrapper.scrollLeft += delta;
  }

  $effect(() => {
    void messages;
    // Toggling alignment swaps which DOM child is visually leftmost vs
    // rightmost, so fade widths and edge detection have to recompute.
    void alignment;
    if (!wrapper) return;
    const ro = new ResizeObserver(() => {
      updateFade();
      pinToEnd();
    });
    ro.observe(wrapper);
    for (const child of Array.from(wrapper.children)) ro.observe(child);
    const onScroll = () => {
      updateFade();
      userScrolled = !isAtEnd();
    };
    wrapper.addEventListener("scroll", onScroll, { passive: true });
    // `passive: false` so we can call preventDefault to stop the parent
    // scroll container from also consuming the wheel event.
    wrapper.addEventListener("wheel", onWheel, { passive: false });
    updateFade();
    // Wait one tick so the layout reflows from any messages change before
    // measuring lastElementChild for scrollIntoView.
    void tick().then(pinToEnd);
    return () => {
      ro.disconnect();
      wrapper?.removeEventListener("scroll", onScroll);
      wrapper?.removeEventListener("wheel", onWheel);
    };
  });

  // A substack is a single horizontal row of collapsed bubbles, isolated
  // from neighbouring rows in the parent column. Chain neighbours therefore
  // come purely from DOM adjacency within this row.
  function neighbors(i: number): {
    neighborLeft: boolean;
    neighborRight: boolean;
  } {
    const hasDomPrev = i > 0;
    const hasDomNext = i < messages.length - 1;
    if (alignment === "right") {
      // flex-row-reverse: DOM[i+1] is visually to the LEFT of DOM[i].
      return { neighborLeft: hasDomNext, neighborRight: hasDomPrev };
    }
    return { neighborLeft: hasDomPrev, neighborRight: hasDomNext };
  }

  // Capture-phase click handler: clicking anywhere on a collapsed bubble
  // expands it (sets `expansionState[id] = true`). The parent group's
  // segment grouping then promotes the bubble to its own row, splitting
  // this substack. `stopPropagation` prevents the bubble's own
  // chevron-toggle (or any descendant click handler) from also firing.
  function handleClickCapture(e: MouseEvent) {
    if (!wrapper) return;
    let node: Element | null = e.target as Element | null;
    while (node && node !== wrapper && node.parentElement !== wrapper) {
      node = node.parentElement;
    }
    if (!node || node === wrapper) return;
    const i = Array.from(wrapper.children).indexOf(node);
    if (i < 0) return;
    const msg = messages[i];
    // Loading sentinel has no Expandable body; clicking shouldn't promote
    // it to a standalone row.
    if (msg.role === "loading") return;
    if (msg.id === undefined) return;
    e.stopPropagation();
    expansionState.set(msg.id, true);
  }
</script>

<MessageStackView
  count={messages.length}
  {alignment}
  {fadeLeft}
  {fadeRight}
  onClickCapture={handleClickCapture}
  bind:wrapper
>
  {#snippet bubble(i)}
    {@const n = neighbors(i)}
    {@render item({
      msg: messages[i],
      idx: baseIdx + i,
      neighborLeft: n.neighborLeft,
      neighborRight: n.neighborRight,
    })}
  {/snippet}
</MessageStackView>
