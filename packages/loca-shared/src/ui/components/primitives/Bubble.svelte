<script lang="ts">
  import { type Snippet, untrack } from "svelte";
  import type { Alignment } from "../../types.ts";
  import type { BubbleCorner } from "../../merge.ts";
  import { bubbleGapExpr, useUiContext } from "../../context.ts";
  import { longpress } from "../../actions/longpress.ts";

  const ui = useUiContext();
  // The touch shell renders bubbles flat: the drop shadow and frosted halo are
  // desktop-window decorations (there is no transparent backdrop to frost on an
  // opaque app), and the roomy desktop padding wastes width on a phone. So on
  // mobile the shadow/halo are dropped and padding is slim + equal on both axes.
  const mobile = ui.platform === "mobile";

  type Accent = "blue" | "green" | "red" | "yellow" | "purple";

  let {
    selectedAlignment,
    size = "large",
    bgClass = "bg-surface",
    accent = undefined,
    extraClass = "",
    active = false,
    pulse = false,
    fullWidth = false,
    borderColorClass = "",
    neighborLeft = false,
    neighborRight = false,
    flatCorners = [],
    overlapTop = false,
    onWidth = undefined,
    progress,
    progressFillBgClass = "bg-default-800",
    onclick,
    oncontextmenu,
    onlongpress,
    ondblclick,
    children,
  } = $props<{
    selectedAlignment: Alignment;
    /** "large" = roomy (px-5 py-4), used for primary message content.
     *  "small" = compact (px-3 py-2 + min-h-8), used for header-only / loading
     *  bubbles where the body is just an Expandable header or a spinner.
     *  Small bubbles enforce a uniform 2rem floor so a header-only bubble,
     *  a loading spinner bubble, and a bubble showing progress all share the
     *  same vertical extent in a stack row. */
    size?: "small" | "large";
    bgClass?: string;
    /** Retint the WHOLE bubble to an accent hue by setting `--default-base` on
     *  the body, so every nested `-default-` color (the `bg-surface` fill,
     *  insets, text, borders, hover/focus/selected states) re-resolves to the
     *  accent instead of neutral gray, the same way agent/user bubbles are soft
     *  hue tints. Code surfaces (`--code-bg*`) stay neutral by design. Keep
     *  `bgClass` at the default `bg-surface` so a themed bubble sits at surface
     *  lightness like the agent/user bubbles. Undefined = neutral. */
    accent?: Accent;
    extraClass?: string;
    active?: boolean;
    pulse?: boolean;
    /** Stretch to the parent's full width instead of hugging content (`w-fit`).
     *  Used by the mobile composer so its control row spans the screen and the
     *  send button is never clipped by the desktop-tuned max-width. */
    fullWidth?: boolean;
    borderColorClass?: string;
    /** Visual-left side has another bubble in the same stack row; collapse
     *  the left-side rounding to `md` to signal adjacency. Composes with the
     *  alignment override (left-aligned bubbles already get `rounded-l-small`). */
    neighborLeft?: boolean;
    /** Visual-right side has another bubble in the same stack row. */
    neighborRight?: boolean;
    /** Corners to square off (radius 0) when this bubble merges flush into a
     *  stacked neighbor (CoreBar under the SessionBar / Settings panel). */
    flatCorners?: BubbleCorner[];
    /** This is the lower bubble of a merged pair: pull it up by one padding plus
     *  the column gap so its top padding overlaps the neighbor's bottom padding
     *  with no seam, and clip its top shadow/halo (which would otherwise cast onto
     *  the bubble above). */
    overlapTop?: boolean;
    /** Report this bubble's rendered content width (via a ResizeObserver on the
     *  body). Used to decide which of two merging bubbles is the narrower one. */
    onWidth?: (width: number) => void;
    /** Progress visualisation rendered AS the bubble background. When set, the
     *  WHOLE bubble reads inverted (a solid `progressFillBgClass` fill under an
     *  `invert()`-ed copy of the content) and the progress bar is an
     *  un-inverted WINDOW showing the original colours where the bar has
     *  reached. The overlay and window span the full bubble height, so a bubble
     *  of any size (a one-line header or a tall expanded body) is covered edge
     *  to edge. `undefined` = no progress; `null` = indeterminate sweeping
     *  window; number = percent (0..100). When progress clears, the window
     *  sweeps to 100% (the inversion lifts off the whole bubble) before the
     *  layers unmount. */
    progress?: number | null;
    /** Override for the inverted-overlay fill colour (the bubble's colour while
     *  inverted). Defaults to `bg-default-800` to pair with the default
     *  `bg-surface` track. */
    progressFillBgClass?: string;
    onclick?: (e: MouseEvent) => void;
    oncontextmenu?: (e: MouseEvent) => void;
    /** Touch long-press handler (the mobile stand-in for right-click). Wired to
     *  the body via the shared `longpress` action; touch-only, so desktop mouse
     *  input is unaffected. */
    onlongpress?: () => void;
    ondblclick?: (e: MouseEvent) => void;
    children: Snippet;
  }>();

  // Accent retint: a CSS color value (not a utility class), so a template
  // literal is fine here. Set on the body element, it re-resolves the whole
  // `--default-*` ladder to the accent hue for the body and every descendant.
  let defaultBaseOverride = $derived(accent ? `var(--accent-${accent}-base)` : undefined);

  // Slim, equal padding on mobile (roughly the default shadow distance); the
  // roomier asymmetric desktop padding stays on the desktop shell.
  let paddingClass = $derived(
    mobile ? (size === "small" ? "p-2" : "p-3") : size === "small" ? "px-3 py-2" : "px-5 py-4",
  );
  let minHClass = $derived(size === "small" ? "min-h-8" : "");
  let hasProgress = $derived(progress !== undefined);
  let percent = $derived(
    typeof progress === "number" ? Math.max(0, Math.min(100, progress)) : null,
  );
  let isDeterminate = $derived(hasProgress && percent !== null);
  // Progress lifecycle. The overlay + window layers stay mounted through an
  // exit animation after `progress` clears, so the un-inverted window can sweep
  // to 100% (the inversion lifts off the whole bubble) before they unmount.
  // Initialised from the current prop so a server-rendered bubble that already
  // has progress paints its layers without waiting for an effect.
  let mounted = $state(untrack(() => progress !== undefined));
  let exiting = $state(false);
  $effect(() => {
    if (hasProgress) {
      mounted = true;
      exiting = false;
      return;
    }
    if (!mounted) return;
    exiting = true;
    const done = setTimeout(() => {
      mounted = false;
      exiting = false;
    }, 500);
    return () => clearTimeout(done);
  });
  // Grow-from-zero on spawn. A CSS transition only animates a value that
  // changes after the element exists, so a window that mounts already at e.g.
  // 50% (a tool call that spawns mid-run, or a pending->running flip from the
  // indeterminate sweep to a fresh determinate element) would otherwise snap to
  // half-width. Paint the window at 0 for one frame, then grow to the real
  // value so `transition-[clip-path]` animates the spawn. The effect keys on
  // `isDeterminate` only (not `percent`), so later progress updates animate
  // normally without re-triggering the reset. rAF/effects never run during SSR,
  // so the static website build is unaffected.
  let grown = $state(false);
  $effect(() => {
    if (!isDeterminate) {
      grown = false;
      return;
    }
    grown = false;
    let raf2 = 0;
    const raf1 = requestAnimationFrame(() => {
      raf2 = requestAnimationFrame(() => {
        grown = true;
      });
    });
    return () => {
      cancelAnimationFrame(raf1);
      cancelAnimationFrame(raf2);
    };
  });
  // Width of the un-inverted progress window, anchored at the bubble's origin
  // edge. During the exit animation it sweeps to 100% so the bubble fully
  // returns to its normal colours before the layers unmount.
  let displayPercent = $derived(exiting ? 100 : grown ? percent : 0);
  // Right-aligned bubbles fill the window from right to left so the motion
  // mirrors the bubble's anchor edge. The indeterminate sweep and the window's
  // clip-path use mirrored keyframes / inset values.
  let isRight = $derived(selectedAlignment === "right");

  // Frosted-edge halo: render exactly N concentric blur rings (0 when the
  // effect is off). The ring count is a single global knob read from the UI
  // context (the client backs it with the appearance settings; the website
  // supplies a static value); rendering exactly N layers (vs a fixed max) is
  // what makes the count an actual perf control. Geometry per ring is computed
  // in CSS from the `--ring-index` / `--ring-count` custom props set below.
  let ringCount = $derived(mobile ? 0 : ui.bubbleBlurEnabled ? (ui.bubbleBlurRings ?? 3) : 0);

  // Per-corner radius, exported to CSS so the body, the shadow layer, and the
  // halo rings all track the same alignment / neighbor / merge corner flattening.
  // A screen-edge-anchored (or stack-neighbor) side collapses to the small
  // radius; a merged corner squares off entirely.
  let leftSmall = $derived(selectedAlignment === "left" || neighborLeft);
  let rightSmall = $derived(selectedAlignment === "right" || neighborRight);
  let flat = $derived(new Set<BubbleCorner>(flatCorners));
  function cornerRadius(corner: BubbleCorner, small: boolean): string {
    if (flat.has(corner)) return "0px";
    return small ? "var(--rounded-small)" : "var(--rounded-large)";
  }
  let radiusTL = $derived(cornerRadius("tl", leftSmall));
  let radiusTR = $derived(cornerRadius("tr", rightSmall));
  let radiusBR = $derived(cornerRadius("br", rightSmall));
  let radiusBL = $derived(cornerRadius("bl", leftSmall));
  let bodyRadius = $derived(`${radiusTL} ${radiusTR} ${radiusBR} ${radiusBL}`);

  // Merge overlap: pull the lower bubble up by the column gap (removing it) plus
  // one small-bubble padding (0.5rem, so its top padding overlaps the neighbor's
  // bottom padding). The flex item is a BFC, so this negative margin overlaps the
  // neighbor above instead of collapsing out. `--bubble-merge-overlap` also drives
  // the shadow/halo top clip (base.css) so it tracks the same amount: the lower
  // bubble sits in a higher stacking context than the neighbor, so its shadow/halo
  // must be clipped to the neighbor's bottom edge or they paint over its surface.
  let overlapAmount = "0.5rem";
  let overlapMargin = $derived(
    overlapTop ? `calc(-1 * (var(--bubble-merge-overlap) + ${bubbleGapExpr(ui)}))` : undefined,
  );

  // Report the body's rendered width so a merging neighbor can tell which bubble
  // is narrower. Skipped when no consumer wants it.
  let bodyEl = $state<HTMLElement>();
  $effect(() => {
    if (!onWidth || !bodyEl) return;
    const el = bodyEl;
    const ro = new ResizeObserver(() => onWidth?.(el.offsetWidth));
    ro.observe(el);
    onWidth(el.offsetWidth);
    return () => ro.disconnect();
  });
</script>

<!-- Positioning + halo-containment wrapper. The body below clips its own
     overflow, so the drop shadow and the blur halo (which extend beyond the
     bubble) live out here as sibling layers behind the body. Alignment
     margins live on the wrapper so it stays the laid-out element in the
     parent flex column.

     Deliberately NOT `isolate`: the shadow/halo layers sit at z-0 and the
     body at z-10 in the SURROUNDING stacking context, so every bubble's
     shadow paints below every bubble's body and can never cover a
     neighboring bubble. (WebKit's backdrop-filter mis-sampling under a
     transformed ancestor is handled by `bubble-body-promote`, which keeps
     the body on its own compositing layer.) -->
<div
  class="relative pointer-events-none {fullWidth ? 'w-full' : 'w-fit'}"
  class:mr-auto={selectedAlignment === "left"}
  class:ml-auto={selectedAlignment === "right"}
  class:mx-auto={selectedAlignment === "center"}
  style:--bubble-radius-tl={radiusTL}
  style:--bubble-radius-tr={radiusTR}
  style:--bubble-radius-br={radiusBR}
  style:--bubble-radius-bl={radiusBL}
  style:--bubble-merge-overlap={overlapTop ? overlapAmount : undefined}
  style:margin-top={overlapMargin}
>
  {#if !mobile}
    <div
      class="bubble-shadow absolute inset-0 z-0"
      class:merge-clip-top={overlapTop}
      aria-hidden="true"
    ></div>
  {/if}
  {#each Array(ringCount) as _, i (i)}
    <div
      class="bubble-halo"
      class:merge-clip-top={overlapTop}
      style="--ring-index: {i}; --ring-count: {ringCount}"
      aria-hidden="true"
    ></div>
  {/each}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div
    bind:this={bodyEl}
    {onclick}
    {oncontextmenu}
    {ondblclick}
    use:longpress={onlongpress}
    role={onclick ? "presentation" : undefined}
    style:--default-base={defaultBaseOverride}
    style:border-radius={bodyRadius}
    class="bubble-body {bgClass} {minHClass} relative z-10 overflow-hidden {fullWidth
      ? 'w-full max-w-full'
      : 'w-fit max-w-[calc(100vw-5rem)]'} break-words transition-all duration-100 border-solid pointer-events-auto {borderColorClass}"
    class:bubble-body-promote={ringCount > 0}
    class:bubble-longpress={onlongpress}
    class:border-l-8={selectedAlignment === "left" && active}
    class:border-l-0={selectedAlignment === "left" && !active}
    class:border-r-8={selectedAlignment === "right" && active}
    class:border-r-0={selectedAlignment === "right" && !active}
    class:border-b-8={selectedAlignment === "center" && active}
    class:border-b-0={selectedAlignment === "center" && !active}
    class:bubble-border-pulse={active && pulse}
  >
    <div class="relative z-10 {paddingClass} {extraClass}">
      {@render children()}
    </div>
    {#if mounted}
      <!-- Inverted overlay: the whole bubble reads inverted. Split in two:
           a solid `progressFillBgClass` fill and, over it, an
           `invert(1) hue-rotate(180deg)` copy of the content (flipping text,
           icons and inline pills to their perceptual inverse in one shot, no
           need to thread invert-color props through every descendant). The fill
           is bled 1px past every edge so the body's rounded `overflow-hidden`
           clip - not the fill's own antialiased edge - defines the boundary;
           otherwise a hairline of the original (light) background shows at the
           edges, glaring at full-inversion's max contrast. The content stays at
           the true bubble box so the inverted copy lines up with the base and
           window copies (a bled content box would reflow its text by ~1px). The
           un-inverted window below sits ON TOP and reveals the original colours
           wherever the bar has reached. -->
      <div
        class="absolute z-20 pointer-events-none {progressFillBgClass}"
        style="inset: -1px"
        aria-hidden="true"
      ></div>
      <div class="absolute inset-0 z-20 pointer-events-none" aria-hidden="true">
        <div class="bubble-progress-invert absolute inset-0 {paddingClass} {extraClass}">
          {@render children()}
        </div>
      </div>
      <!-- Un-inverted window: a copy of the bubble's normal colours clipped to
           the bar's reached region, so the filled part of the progress reads as
           the original (non-inverted) bubble. `transition-[clip-path]` animates
           the determinate grow and the sweep to 100% on exit; the indeterminate
           state instead slides an animated mask (a clip-path animated via
           @keyframes mis-composites in WebKit when it overlaps the inverted
           overlay below, so the band would collapse to the bubble's centre).

           The clip and the filter are split across separate elements ON
           PURPOSE. This window carries ONLY the clip (no filter); the overlay
           above carries ONLY the filter (no clip). They must not share one
           element: WebKit mis-renders an element that has BOTH a `clip-path`
           and a `filter` when it lives inside a `transform`-promoted ancestor
           (the `bubble-body-promote` compositing layer), clipping the filtered
           content to a stale sub-rect even where the clip-path reveals it.

           Children are rendered a second extra time here (base + overlay +
           window); Svelte's bind:expanded on the shared parent state keeps
           every Expandable copy in lockstep, and pointer-events:none routes all
           clicks to the base (lower, un-clipped) copy. -->
      <div
        class="absolute inset-0 z-30 pointer-events-none {bgClass} transition-[clip-path] duration-500"
        class:bubble-progress-window-indet={percent === null && !exiting && !isRight}
        class:bubble-progress-window-indet-rtl={percent === null && !exiting && isRight}
        style:clip-path={percent === null && !exiting
          ? undefined
          : isRight
            ? `inset(0 0 0 calc(100% - ${displayPercent}%))`
            : `inset(0 calc(100% - ${displayPercent}%) 0 0)`}
        aria-hidden="true"
      >
        <div class="absolute inset-0 {paddingClass} {extraClass}">
          {@render children()}
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  /* Pulses the border color between its current value and transparent. With
     the default `background-clip: border-box`, the bubble's own bg paints
     under the border area, so the transparent half reveals the bg color,
     producing a pulse between `border-<hue>-400` and `bg-<hue>-300` with no
     extra color plumbing required. */
  .bubble-border-pulse {
    animation: bubble-border-pulse 0.5s ease-in-out infinite alternate;
  }
  @keyframes bubble-border-pulse {
    to {
      border-color: transparent;
    }
  }

  /* Force the body onto its own compositing layer whenever halo rings exist.
     The halo's `backdrop-filter` is meant to frost the desktop behind this
     transparent window, but WebKit has a bug: under an ancestor with a
     `transform` / `will-change: transform` (the panel slide wrapper), a
     backdrop-filter samples the FOREGROUND of its compositing group instead of
     the true backdrop, so the halo blurs the bubble's own content. Content that
     already lives on its own layer is excluded from that bad sample (which is
     why an inner `overflow-y-auto` scroll area stays sharp while the rest
     blurs). Promoting the whole body to its own layer makes ALL of it escape
     the same way, leaving the halo to blur only the desktop behind it. */
  .bubble-body-promote {
    transform: translateZ(0);
  }

  /* When a bubble carries a touch long-press handler (its mobile stand-in for
     right-click, opening the action sheet), iOS otherwise fires its OWN
     long-press gestures on top: the native callout menu (Copy / Select All) and
     a text selection that flashes the bubble blue. Suppress both so the hold
     resolves to our action sheet alone. Scoped to coarse pointers so desktop
     text stays selectable; the mobile shell never selects bubble text anyway. */
  @media (pointer: coarse) {
    .bubble-longpress {
      -webkit-touch-callout: none;
      -webkit-user-select: none;
      user-select: none;
    }
  }

  .bubble-progress-invert {
    filter: invert(1) hue-rotate(180deg);
    pointer-events: none;
  }

  /* Indeterminate sweep: a hard-edged band of the un-inverted window slides
     across the inverted bubble, widening to half the bubble at the midpoint and
     collapsing to nothing at each end (`ease-in` into the midpoint, `ease-out`
     out of it) - the original determinate-fill motion.

     Driven by an animated MASK whose two opaque edges (`--bar-from`/`--bar-to`,
     registered with @property so they interpolate as <percentage>s) move and
     spread, NOT by an animated `clip-path`: WebKit (and Chromium) mis-composite
     an element whose `clip-path` is animated via @keyframes when it overlaps a
     sibling layer (here the full-cover inverted overlay) - the animated-clip
     window collapses and only the dark overlay shows, so the bar looked trapped
     in a central band. An animated mask composites correctly. (Static clip-path
     and clip-path TRANSITIONS are unaffected, so the determinate bar above
     keeps them.) The gradient jumps straight from transparent to opaque at each
     edge, so the band reads as a crisp rectangle, not a feathered blur. */
  @property --bar-from {
    syntax: "<percentage>";
    inherits: false;
    initial-value: 0%;
  }
  @property --bar-to {
    syntax: "<percentage>";
    inherits: false;
    initial-value: 0%;
  }
  .bubble-progress-window-indet,
  .bubble-progress-window-indet-rtl {
    --bar-from: 0%;
    --bar-to: 0%;
    -webkit-mask-image: linear-gradient(
      to right,
      transparent var(--bar-from),
      #000 var(--bar-from),
      #000 var(--bar-to),
      transparent var(--bar-to)
    );
    mask-image: linear-gradient(
      to right,
      transparent var(--bar-from),
      #000 var(--bar-from),
      #000 var(--bar-to),
      transparent var(--bar-to)
    );
  }
  .bubble-progress-window-indet {
    animation: bubble-progress-window-indet 1.6s linear infinite;
  }
  @keyframes bubble-progress-window-indet {
    0% {
      --bar-from: 0%;
      --bar-to: 0%;
      animation-timing-function: ease-in;
    }
    50% {
      --bar-from: 25%;
      --bar-to: 75%;
      animation-timing-function: ease-out;
    }
    100% {
      --bar-from: 100%;
      --bar-to: 100%;
    }
  }

  /* Right-to-left mirror: the band sweeps from the right edge to the left, so
     the right-aligned bubble's progress motion mirrors its anchor edge. */
  .bubble-progress-window-indet-rtl {
    animation: bubble-progress-window-indet-rtl 1.6s linear infinite;
  }
  @keyframes bubble-progress-window-indet-rtl {
    0% {
      --bar-from: 100%;
      --bar-to: 100%;
      animation-timing-function: ease-in;
    }
    50% {
      --bar-from: 25%;
      --bar-to: 75%;
      animation-timing-function: ease-out;
    }
    100% {
      --bar-from: 0%;
      --bar-to: 0%;
    }
  }
</style>
