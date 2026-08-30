// Animation timing constants and motion primitives shared by every Loca UI
// surface. Every `durationMs` here is resolved by the caller: the client through
// the UI context's settings-aware `animationDurationMs`, the website via
// BASE_MS.

import type { Alignment } from "./types.ts";

/** cubic-bezier(0.4, 0, 0.2, 1): material "standard" easing. */
export const CSS_EASING = "cubic-bezier(0.4, 0, 0.2, 1)";

/** Base transition length in milliseconds. */
export const BASE_MS = 267;

/** Button-like interaction feedback (hover/press color shifts). Kept separate
 *  from BASE_MS so micro-feedback stays snappy. Paired with CSS_EASING by the
 *  `transition-interactive` UnoCSS shortcut. */
export const INTERACTIVE_MS = 120;

/** The `use:ripple` press splash. Longer than INTERACTIVE_MS so the circle
 *  reads as it expands and fades. */
export const RIPPLE_MS = 420;

/**
 * Play an explicit transform keyframe with WAAPI, resolving when it lands and
 * leaving the END transform as the element's inline base.
 *
 * Use this, NOT a CSS transition: a transition interpolates from the CURRENT
 * COMPUTED transform, so a slide sequenced across timers intermittently starts
 * from whatever mid value the WebView was still painting. WAAPI keyframes always
 * start at `from`.
 *
 * `durationMs <= 0`, or a WebView without `animate`, sets the end state
 * instantly. A cancelled animation resolves rather than rejecting.
 */
export function animateTransform(
  el: HTMLElement,
  from: string,
  to: string,
  durationMs: number,
  easing: string = CSS_EASING,
): Promise<void> {
  const base = to === "none" ? "" : to;
  if (durationMs <= 0 || typeof el.animate !== "function") {
    el.style.transform = base;
    return Promise.resolve();
  }
  // `fill: "both"` makes the animation own the transform from creation: `from`
  // shows immediately (no flash at the resting position) and `to` is held after
  // finishing, so writing the inline base then cancelling leaves no gap.
  const anim = el.animate([{ transform: from }, { transform: to }], {
    duration: durationMs,
    easing,
    fill: "both",
  });
  const settle = () => {
    el.style.transform = base;
    try {
      anim.cancel();
    } catch {
      // Already cancelled (node unmounted mid-slide); nothing to finalize.
    }
  };
  // Resolve on cancel too (never reject): the node may unmount mid-slide.
  return anim.finished ? anim.finished.then(settle, settle) : Promise.resolve();
}

/** cubic-bezier(0.4, 0, 0.2, 1) sampled as a JS easing for rAF-driven motion. */
function easeInOut(t: number): number {
  return t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2;
}

export interface ExpandHandle {
  cancel(): void;
}

/**
 * Drive a per-frame max-height + opacity expand/collapse. Per frame (not a CSS
 * transition to a baked height) so the target tracks `scrollHeight` as async
 * content fills in. `durationMs <= 0` snaps to the end state: "open" clears the
 * styles (natural height), "close" ends fully hidden.
 */
export function runExpand(
  el: HTMLElement,
  direction: "open" | "close",
  durationMs: number,
  onComplete?: () => void,
): ExpandHandle {
  const settle = () => {
    const open = direction === "open";
    el.style.maxHeight = open ? "" : "0";
    el.style.overflow = open ? "" : "hidden";
    el.style.opacity = open ? "" : "0";
    onComplete?.();
  };
  if (durationMs <= 0) {
    settle();
    return { cancel: () => {} };
  }

  let cancelled = false;
  let rafId = 0;
  const startTime = performance.now();

  const frame = (now: number) => {
    if (cancelled) return;
    const elapsed = (now - startTime) / durationMs;
    const t = direction === "open" ? Math.min(1, elapsed) : Math.max(0, 1 - elapsed);
    if ((direction === "open" && t >= 1) || (direction === "close" && t <= 0)) {
      settle();
      return;
    }
    const p = easeInOut(t);
    const height = el.scrollHeight;
    el.style.maxHeight = `${height * p}px`;
    el.style.overflow = "hidden";
    el.style.opacity = `${p}`;
    rafId = requestAnimationFrame(frame);
  };

  rafId = requestAnimationFrame(frame);
  return {
    cancel() {
      cancelled = true;
      cancelAnimationFrame(rafId);
    },
  };
}

/**
 * A message bubble's entry: one eased rAF phase that grows max-height (so
 * neighbours below don't jump) while sliding the bubble in from the
 * alignment-appropriate edge (center slides up from below). No fade, no outro.
 * For the duration the node sits at z-index -1, so a bubble entering a stack row
 * emerges from UNDER its neighbours. `centerDirection: "down"` flips the center
 * axis to enter from above (the session bar); no effect on left/right.
 * `delayMs` holds the bubble offscreen first, to stagger a shared mount flush.
 */
export function runMessageEnter(
  node: HTMLElement,
  alignment: Alignment,
  durationMs: number,
  delayMs = 0,
  centerDirection: "up" | "down" = "up",
): void {
  if (durationMs <= 0) return;

  // Sign of the center-axis travel: +1 enters from below (slides up), -1 enters
  // from above (slides down).
  const centerSign = centerDirection === "down" ? -1 : 1;

  const height = node.offsetHeight;

  const setOffscreen = () => {
    // Scoped to the animation: will-change makes the row a stacking context so
    // a bubble's shadow can paint over its neighbours; position + z-index -1
    // keeps the entering bubble below every settled sibling.
    node.style.willChange = "transform";
    node.style.position = "relative";
    node.style.zIndex = "-1";
    node.style.maxHeight = "0";
    node.style.overflow = "hidden";
    if (alignment === "left") node.style.transform = "translateX(-100%)";
    else if (alignment === "right") node.style.transform = "translateX(100%)";
    else node.style.transform = `translateY(${centerSign * 100}%)`;
  };

  setOffscreen();
  void node.offsetHeight;

  let startTime = 0;
  const frame = (now: number) => {
    if (startTime === 0) startTime = now;
    const t = Math.min(1, (now - startTime) / durationMs);
    if (t >= 1) {
      node.style.willChange = "";
      node.style.position = "";
      node.style.zIndex = "";
      node.style.maxHeight = "";
      node.style.overflow = "";
      node.style.transform = "";
      return;
    }
    const p = easeInOut(t);
    const travel = 100 * (1 - p);
    let transform: string;
    if (alignment === "left") transform = `translateX(${-travel}%)`;
    else if (alignment === "right") transform = `translateX(${travel}%)`;
    else transform = `translateY(${centerSign * travel}%)`;
    node.style.maxHeight = `${height * p}px`;
    node.style.overflow = "hidden";
    node.style.transform = transform;
    requestAnimationFrame(frame);
  };

  if (delayMs > 0) setTimeout(() => requestAnimationFrame(frame), delayMs);
  else requestAnimationFrame(frame);
}

/** The nearest scrollable ancestor of `el`, or null. */
function nearestScroller(el: HTMLElement): HTMLElement | null {
  for (let n = el.parentElement; n; n = n.parentElement) {
    const o = getComputedStyle(n);
    if (/(auto|scroll)/.test(`${o.overflowY} ${o.overflowX}`)) return n;
  }
  return null;
}

/**
 * The settings panel's content-layer slide-swap: a TANDEM cross-slide where the
 * outgoing and incoming content move together, not out-then-wait-then-in. The
 * outgoing content is snapshotted into a fixed, clipped ghost overlay that
 * slides away over the top while `swap` mutates the real layer underneath.
 *
 *   axis "y" + outSign 1  => current leaves downward, new enters from above.
 *   axis "x" + outSign -1 => current leaves leftward, new enters from the right.
 *
 * `durationMs <= 0`, no element, or no scroll host runs `swap` with no motion.
 * `swap` may be async (the client awaits `tick()` so the new DOM exists first).
 */
export async function slideSwap(
  el: HTMLElement | undefined,
  opts: {
    axis: "x" | "y";
    outSign: 1 | -1;
    durationMs: number;
    swap: () => void | Promise<void>;
  },
): Promise<void> {
  const { axis, outSign, durationMs, swap } = opts;
  const host = el?.parentElement;
  const scroller = el ? nearestScroller(el) : null;
  if (!el || durationMs <= 0 || !host || !scroller) {
    await swap();
    if (el) el.style.transform = "";
    return;
  }
  const fn = axis === "y" ? "translateY" : "translateX";

  // Clip the ghost to exactly what is on screen now, so a scrolled tall group
  // does not flash its hidden tail.
  const sc = scroller.getBoundingClientRect();
  const er = el.getBoundingClientRect();
  // A clone parked at the body root leaves behind any ancestor
  // `transform: scale()` (the website scales the app stage into its showcase
  // frame), so recover that visual scale (on-screen px over layout px) and
  // re-apply it to a wrapper inside the clip. Every offset below is then in the
  // outgoing layer's unscaled px, the space `el` animates in. `offsetWidth` is
  // integer-rounded, so snap a near-1 measurement to a plain wrapper.
  const measured = el.offsetWidth > 0 ? er.width / el.offsetWidth : 1;
  const scale = Math.abs(measured - 1) < 0.01 ? 1 : measured;
  const clip = document.createElement("div");
  clip.setAttribute("aria-hidden", "true");
  clip.style.cssText =
    `position:fixed;top:${sc.top}px;left:${sc.left}px;width:${sc.width}px;` +
    `height:${sc.height}px;overflow:hidden;pointer-events:none;z-index:40;`;
  const scaler = document.createElement("div");
  scaler.style.cssText =
    `position:absolute;top:0;left:0;width:${sc.width / scale}px;` +
    `height:${sc.height / scale}px;` +
    (scale === 1 ? "" : `transform:scale(${scale});transform-origin:0 0;`);
  const ghost = el.cloneNode(true) as HTMLElement;
  // Drop any h-full / flex sizing so the ghost is exactly content-tall, then
  // place it at the content's current on-screen offset (negative when scrolled).
  ghost.style.cssText =
    `position:absolute;margin:0;height:auto;width:${er.width / scale}px;` +
    `top:${(er.top - sc.top) / scale}px;left:${(er.left - sc.left) / scale}px;`;
  scaler.appendChild(ghost);
  clip.appendChild(scaler);
  // The document root, NOT the settings tree: the panel layer's
  // `will-change: transform` would make it the containing block for
  // `position: fixed` and offset the overlay by the alignment margin.
  document.body.appendChild(clip);

  // Exit travel = the greater of the content's own size and one viewport, so a
  // tall group clears whole (tail included) and a short one still slides the
  // full view. The incoming content enters from one viewport away.
  const viewPx = (axis === "y" ? sc.height : sc.width) / scale;
  const contentPx = axis === "y" ? ghost.scrollHeight : ghost.scrollWidth;
  const outPx = Math.max(contentPx, viewPx);
  const inPx = viewPx;

  await swap();

  await Promise.all([
    animateTransform(ghost, `${fn}(0px)`, `${fn}(${outSign * outPx}px)`, durationMs),
    animateTransform(el, `${fn}(${-outSign * inPx}px)`, "none", durationMs),
  ]);

  clip.remove();
  el.style.transform = "";
}

// Sidebar labels share one expansion SPEED, not one duration: the time scales
// with each label's width so every row sweeps at the same px/ms.
// `LABEL_REF_WIDTH` animates in exactly `BASE_MS`; `LABEL_MIN_MS` floors
// near-empty labels above a sub-frame duration. No upper clamp (it made short
// labels lag).
const LABEL_REF_WIDTH = 96;
const LABEL_MIN_MS = 50;

/** The natural (pre-speed-policy) collapse duration for a label `width` px wide. */
export function labelCollapseBaseMs(width: number): number {
  return Math.max(LABEL_MIN_MS, (Math.max(0, width) / LABEL_REF_WIDTH) * BASE_MS);
}

/**
 * Slide a label between visible (natural width, opacity 1) and collapsed (width
 * 0, opacity 0). Width is measured live via `scrollWidth` so dynamic content
 * animates correctly. `resolveDuration` maps the width-scaled ms to the app's
 * effective duration; returning 0 collapses/expands instantly.
 */
export function collapseLabel(
  el: HTMLElement,
  collapsed: boolean,
  resolveDuration: (baseMs: number) => number,
): void {
  if (collapsed) {
    const w = el.scrollWidth || el.getBoundingClientRect().width;
    const dur = resolveDuration(labelCollapseBaseMs(w));
    if (dur <= 0) {
      el.style.transition = "";
      el.style.width = "0px";
      el.style.opacity = "0";
      return;
    }
    const trans = `width ${dur}ms ${CSS_EASING}, opacity ${dur}ms ${CSS_EASING}`;
    el.style.transition = "none";
    el.style.width = `${w}px`;
    el.style.opacity = "1";
    void el.offsetHeight;
    el.style.transition = trans;
    el.style.width = "0px";
    el.style.opacity = "0";
  } else {
    el.style.transition = "none";
    el.style.width = "auto";
    el.style.opacity = "1";
    const w = el.scrollWidth;
    const dur = resolveDuration(labelCollapseBaseMs(w));
    if (dur <= 0) {
      el.style.width = "";
      return;
    }
    const trans = `width ${dur}ms ${CSS_EASING}, opacity ${dur}ms ${CSS_EASING}`;
    el.style.width = "0px";
    el.style.opacity = "0";
    void el.offsetHeight;
    el.style.transition = trans;
    el.style.width = `${w}px`;
    el.style.opacity = "1";
    // Drop the explicit width once the transition finishes so the span can
    // resize naturally if its content later changes.
    setTimeout(() => {
      if (el.style.width === `${w}px`) {
        el.style.transition = "";
        el.style.width = "";
      }
    }, dur);
  }
}
