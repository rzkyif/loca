<script lang="ts">
  import type { Snippet } from "svelte";
  import Bubble from "@loca/shared/ui/components/primitives/Bubble.svelte";

  // One labeled frame in the component gallery: a `Component · scenario` caption
  // over a render area painted as a dim focus grid on `bg-surface`, exactly the
  // homepage showcase / manual demo recipe (see DemoFrame.svelte). Like those
  // frames the card is a DOUBLE flip: `.demo-frame` renders the chrome + grid in
  // the opposite site theme, and a `.demo-unflip` wrapper restores the website
  // theme for the rendered component, so in dark mode the card reads light while
  // the component inside reads dark (and vice versa) - the component is shown the
  // way it actually ships. `mb-4` plus, for masonry tiles, `break-inside-avoid`
  // place it in the column flow.
  //
  // Each card is addressable: its label is an anchor to a slug id, so following
  // it sets that hash and the URL changes (copyable/shareable). Only the label
  // links; the card body is NOT clickable, so interacting with a rendered control
  // never navigates. No clipboard write (too disruptive); the URL change is the
  // affordance.
  //
  // surface: wrap the component in a real `Bubble` - the on-surface context that
  //   components shipping on top of a bubble (settings fields, the object
  //   scaffolding, and every primitive) expect, carrying the genuine bubble drop
  //   shadow + frosted halo so the wrap matches what the app paints. Without it a
  //   raw control sits on the flipped card chrome and reads as the opposite theme.
  //   Chat-message Views (already bubbles), modal Views (their own surface), and
  //   the shells render without it.
  // hug: a `surface` modifier for a raw, natural-width component (a permission
  //   request, a button row, a badge) whose content is narrower than the card.
  //   By default the surface bubble fills the card (so a stretch component like a
  //   settings field row or ObjectManager spans it); `hug` instead lets the
  //   bubble shrink to its content (`w-fit`, centered), so it wraps the component
  //   like its name says rather than floating it in a too-wide bubble.
  // backdrop: the component renders its OWN overlay (a Modal/Popover/ActionSheet
  //   open over a dimmed backdrop), so the frame becomes a clipped, transformed
  //   containing block that pins those fixed/absolute layers to the card. The
  //   focus grid still paints behind, and the gallery CSS hides the modal's own
  //   blur scrim so the grid shows through (see site.css `.gallery-frame`).
  // wide: pull the tile out of the masonry column flow so it spans the whole
  //   width, for the large shells that need the room.
  let {
    label,
    surface = false,
    hug = false,
    backdrop = false,
    wide = false,
    children,
  }: {
    label: string;
    surface?: boolean;
    hug?: boolean;
    backdrop?: boolean;
    wide?: boolean;
    children: Snippet;
  } = $props();

  // A stable, shareable anchor id derived from the label.
  const id = $derived(
    "c-" +
      label
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .replace(/^-+|-+$/g, ""),
  );

  // Narrow masonry tiles re-cap their rendered component to the card width (see
  // site.css `.gallery-frame-narrow`); wide shell rows keep the faithful 700px-
  // window proportions, where the bubble background and its drop shadow stay in
  // sync. So only non-wide cards carry the narrow class.
  const frameClass = $derived(wide ? "gallery-frame" : "gallery-frame gallery-frame-narrow");
</script>

<!-- scroll-margin clears the fixed navbar (--chrome-h) plus the 2.5rem top scroll
     fade, with a little gap, so following a label link lands the caption fully
     below the chrome instead of tucked behind it. Tracks the responsive
     --chrome-h the navbar sets. -->
<div
  {id}
  class={`scroll-mt-[calc(var(--chrome-h)+3.5rem)] flex flex-col gap-2 mb-4 ${
    wide ? "" : "break-inside-avoid"
  }`}
>
  <a
    href={`#${id}`}
    class="w-fit font-mono text-xs text-default-500 no-underline transition-interactive hov:text-default-700"
    >{label}</a
  >
  <div
    class={`demo-frame flat-shadow ${frameClass} relative rounded-medium bg-surface min-h-24 overflow-hidden ${
      backdrop ? "" : "flex items-center justify-center"
    }`}
    style={backdrop ? "transform: translateZ(0)" : undefined}
  >
    <!-- The grid stays on the flipped chrome theme; only the component below is
         restored to the website theme by `.demo-unflip`. -->
    <div class="focus-grid-frame" aria-hidden="true"></div>
    {#if backdrop}
      <div class="demo-unflip">
        {@render children()}
      </div>
    {:else}
      <!-- Horizontally scroll content too wide to fit, so it can never spill past
           the card, however narrow the viewport (mobile). `min-w-0` lets the scroll
           area shrink to the card; the inner is `w-fit min-w-full` (fit-content) so
           it CLAMPS to the card for wrappable content (which then wraps, no scroll)
           but GROWS to the content's min-content for unshrinkable content (a wide
           button row, a fixed-width session bubble), left-anchoring it for a clean
           scroll instead of centering it half-off the card. A grown inner also lets
           the surface bubble expand to wrap that content. The `p-6` sits inside the
           scroll area so drop shadows have room before the clip and the scrollbar
           tucks into the padding. -->
      <!-- The scroller stays on the flipped frame theme (no `.demo-unflip`) so its
           scrollbar resolves the same swapped ladder as the card chrome and tracks
           the flip automatically; `.demo-unflip` restores the website theme on the
           inner content so the rendered component matches the site. -->
      <div class="loca-scroll relative z-10 w-full max-w-full min-w-0 overflow-x-auto p-6">
        <div class="demo-unflip flex w-fit min-w-full items-center justify-center">
          {#if surface}
            <!-- Default: `fullWidth` makes the bubble track the inner row: it fills
                 the card for stretch content (a settings field row, ObjectManager)
                 yet grows with the row when wide content (a long button bar) pushes
                 it past the card, so the bubble wraps it. `hug` drops `fullWidth` so
                 the bubble shrinks to its content (`w-fit`, centered by the row's
                 `justify-center`) for a raw, natural-width component narrower than
                 the card, which would otherwise float in a too-wide bubble. -->
            <Bubble selectedAlignment="center" fullWidth={!hug} extraClass="flex justify-center">
              {@render children()}
            </Bubble>
          {:else}
            {@render children()}
          {/if}
        </div>
      </div>
    {/if}
  </div>
</div>
