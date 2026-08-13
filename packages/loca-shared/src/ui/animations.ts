// Animation timing constants shared by every Loca UI surface. The actual
// transition factories (which read the client's animation settings) stay in the
// client; what is shared is the canonical easing curve and base duration so the
// extracted components and the website animate on the same numbers. The client
// resolves a settings-aware duration through the UI context's
// `animationDurationMs`; the website (and any standalone render) uses BASE_MS.

import type { Alignment } from "./types.ts";

/** cubic-bezier(0.4, 0, 0.2, 1): material "standard" easing. */
export const CSS_EASING = "cubic-bezier(0.4, 0, 0.2, 1)";

/** Base transition length in milliseconds. */
export const BASE_MS = 267;

/** The canonical duration for button-like interaction feedback (hover/press
 *  color shifts). Deliberately short and snappy, kept separate from BASE_MS so
 *  micro-feedback stays responsive while larger motions use the longer base.
 *  Paired with CSS_EASING via the `transition-interactive` UnoCSS shortcut. */
export const INTERACTIVE_MS = 120;

/** The canonical press-ripple duration (the `use:ripple` splash that is the
 *  app-wide press affordance). Longer than INTERACTIVE_MS so the circle reads
 *  as it expands and fades, but still snappy. Callers scale it through the UI
 *  context's `animationDurationMs`, which floors to 0 under reduced motion (the
 *  action then renders no ripple). */
export const RIPPLE_MS = 420;

/**
 * Play an explicit transform keyframe on `el` with the Web Animations API,
 * resolving when it lands and leaving the END transform as the element's inline
 * base (so a fill-less finish shows no revert-to-natural flash).
 *
 * Use this, NOT `el.style.transition = ...; el.style.transform = target`. A CSS
 * transition interpolates from the element's CURRENT COMPUTED transform, so when
 * a slide sequences out-then-in across timers the "in" leg intermittently starts
 * from whatever mid value WKWebView was still painting - the "slide starts from
 * the middle (~50%) instead of off-screen" glitch. WAAPI keyframes are explicit:
 * the motion ALWAYS starts at `from`, whatever was last on screen, which removes
 * that failure mode entirely. `element.animate` is already the app's imperative
 * animation primitive (see `actions/ripple`).
 *
 * `durationMs <= 0` sets the end state instantly. A browser-cancelled animation
 * (the node unmounts mid-slide) resolves rather than rejecting, matching the old
 * timer-based callers that always ran to completion.
 *
 * `element.animate` + `Animation.finished` are the only WAAPI surface used here;
 * both shipped together (Chromium 39, Safari 13.1 / iOS 13.4), so every target
 * WebView we ship to supports them (Windows WebView2, Linux WebKitGTK 4.1,
 * iOS >= 14, Android WebView >= Chromium 51, macOS Safari >= 13.1). The one gap
 * is an un-updated macOS 10.13/10.14 (WKWebView follows the installed Safari): if
 * `animate` is missing we set the end transform instantly instead of throwing, so
 * navigation still works - it just lands with no motion on that ancient tail.
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
  // `fill: "both"` makes the animation own the transform from the instant it is
  // created: the element shows `from` immediately (no paint at its resting
  // position first, which for a slide-IN would flash on screen before the motion
  // starts) and holds `to` after finishing. We then write `to` to the inline base
  // and cancel, so the resting transform sticks with no gap between the held
  // frame and the inline value.
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
  // Resolve on cancel too (never reject): the node may unmount mid-slide, and a
  // pre-13.1 WebView could lack the `finished` promise despite having `animate`.
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
 * Drive a per-frame max-height + opacity expand/collapse on `el`, shared so the
 * client (`Expand`/`Expandable`) and the website render the same motion. Per
 * frame (not a CSS transition to a baked height) so the target tracks
 * `scrollHeight` as async content fills in (e.g. markdown that renders a spinner
 * first). `durationMs` is resolved by the caller (client: settings-aware via the
 * UI context; website: `BASE_MS`); `<= 0` snaps to the end state.
 *   - "open":  t 0->1, ends with styles cleared (natural height)
 *   - "close": t 1->0, ends fully hidden
 */
export function runExpand(
  el: HTMLElement,
  direction: "open" | "close",
  durationMs: number,
  onComplete?: () => void,
): ExpandHandle {
  if (durationMs <= 0) {
    if (direction === "open") {
      el.style.maxHeight = "";
      el.style.overflow = "";
      el.style.opacity = "";
    } else {
      el.style.maxHeight = "0";
      el.style.overflow = "hidden";
      el.style.opacity = "0";
    }
    onComplete?.();
    return { cancel: () => {} };
  }

  let cancelled = false;
  let rafId = 0;
  const startTime = performance.now();

  const frame = (now: number) => {
    if (cancelled) return;
    const elapsed = (now - startTime) / durationMs;
    const t = direction === "open" ? Math.min(1, elapsed) : Math.max(0, 1 - elapsed);
    const done = (direction === "open" && t >= 1) || (direction === "close" && t <= 0);
    if (done) {
      if (direction === "open") {
        el.style.maxHeight = "";
        el.style.overflow = "";
        el.style.opacity = "";
      } else {
        el.style.maxHeight = "0";
        el.style.overflow = "hidden";
        el.style.opacity = "0";
      }
      onComplete?.();
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
 * Drive a message bubble's entry: one eased rAF phase that simultaneously grows
 * max-height (so neighbours below don't jump) and slides the bubble in from the
 * alignment-appropriate edge. No fade: the bubble is fully opaque the whole way.
 * For the duration the node is pushed to z-index -1, so a bubble entering a
 * stack row emerges from UNDER its neighbours instead of sweeping over them.
 * Shared so the client transcript and the website showcase animate identically;
 * `durationMs` is resolved by the caller (client: settings-aware via the UI
 * context; website: `BASE_MS`, or 0 under reduced motion). `<= 0` snaps in with
 * no motion. `delayMs` holds the bubble offscreen (collapsed) before the motion
 * starts, used to stagger bubbles that mount in the same flush.
 *   - alignment "left":   slides in from the left  (translateX -100% -> 0)
 *   - alignment "right":  slides in from the right (translateX  100% -> 0)
 *   - alignment "center": slides up from below     (translateY  100% -> 0)
 * `centerDirection` flips the center axis: "down" enters from above instead
 * (translateY -100% -> 0), used by the session bar. It has no effect when the
 * alignment is left/right.
 * No outro: there is no symmetric exit, removals are instant.
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
    // Scoped to the animation: will-change makes the row a stacking context,
    // which lets one bubble's shadow paint over its neighbours (see Bubble's
    // shadow layering). position + z-index -1 keeps the entering bubble below
    // every settled sibling; the node is a plain block, so it needs position.
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

/** The nearest scrollable ancestor of `el` (the visible viewport the sliding
 *  content lives in), or null if none. Lets the swap clip its exit ghost to what
 *  is actually on screen and size the exit travel to the real content. */
function nearestScroller(el: HTMLElement): HTMLElement | null {
  for (let n = el.parentElement; n; n = n.parentElement) {
    const o = getComputedStyle(n);
    if (/(auto|scroll)/.test(`${o.overflowY} ${o.overflowX}`)) return n;
  }
  return null;
}

/**
 * The settings panel's content-layer slide-swap, shared so the client (settings
 * groups + tabs) and the website showcase produce byte-identical motion. It is a
 * TANDEM cross-slide: the outgoing content and the incoming content move together
 * (same direction, same duration), like the mobile page transition, rather than
 * out-then-wait-then-in.
 *
 * The outgoing content is snapshotted into a fixed, clipped ghost overlay so it
 * can slide away over the top while `swap` mutates the real layer underneath and
 * the new content slides in. The ghost travels its FULL content size (not one
 * viewport), so a group taller than the viewport clears entirely - including the
 * tail that was scrolled out of view - instead of leaving a strip behind. The
 * incoming content enters from the opposite edge, one viewport away.
 *
 *   axis "y" + outSign 1  => current leaves downward, new enters from above.
 *   axis "x" + outSign -1 => current leaves leftward, new enters from the right.
 *
 * `durationMs` is resolved by the caller (client: settings-aware `getDuration()`;
 * website: `BASE_MS`); `<= 0`, no element, or no scroll host runs `swap` with no
 * motion. `swap` may be async (the client awaits Svelte's `tick()` so the new DOM
 * exists before the incoming leg starts).
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

  // Snapshot the outgoing content as a fixed, clipped ghost that overlays exactly
  // what is on screen now (so a scrolled tall group does not flash its hidden
  // tail). It exits while the incoming content slides in underneath.
  const sc = scroller.getBoundingClientRect();
  const er = el.getBoundingClientRect();
  // Leaving the settings tree also leaves behind any ancestor `transform: scale()`
  // (the website scales a full-size app stage down to fit its showcase frame), so
  // a clone parked at the body root would render at its full design size over a
  // shrunken UI. Recover that visual scale - on-screen px over layout px - and
  // re-apply it to a wrapper inside the clip. Every offset below is then written
  // in the outgoing layer's own unscaled px, the same space `el` animates in.
  // `offsetWidth` is integer-rounded, so an unscaled tree lands a hair off 1;
  // snap it so the client (never scaled) gets a plain untransformed wrapper.
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
  // Drop any h-full / flex sizing so the ghost is exactly content-tall, then place
  // it at the outgoing content's current on-screen offset (negative when scrolled).
  ghost.style.cssText =
    `position:absolute;margin:0;height:auto;width:${er.width / scale}px;` +
    `top:${(er.top - sc.top) / scale}px;left:${(er.left - sc.left) / scale}px;`;
  scaler.appendChild(ghost);
  clip.appendChild(scaler);
  // Append to the document root, NOT inside the settings tree: the panel layer
  // carries `will-change: transform`, which makes it the containing block for
  // `position: fixed`, so a fixed overlay nested under it would be offset by the
  // alignment margin (its horizontal position would be wrong). At the body root
  // there is no transformed ancestor, so the viewport-space rects above position
  // it correctly.
  document.body.appendChild(clip);

  // Travel = the greater of the content's own size and one viewport, so a group
  // taller than the viewport clears whole (tail included) AND a short group still
  // slides the full view rather than a stub. The incoming content enters from one
  // viewport away, enough to reach rest whatever its height. Unscaled px, matching
  // the ghost's and `el`'s local coordinate space.
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

// Sidebar labels share one expansion SPEED, not one duration: the transition
// time scales with each label's pixel width, so every row sweeps open (and
// closed) at the same px/ms regardless of text length. `LABEL_REF_WIDTH` is the
// width that animates in exactly `BASE_MS`; `LABEL_MIN_MS` floors near-empty
// labels above a sub-frame duration. No upper clamp (that made short labels lag).
const LABEL_REF_WIDTH = 96;
const LABEL_MIN_MS = 50;

/** The natural (pre-speed-policy) collapse duration for a label `width` px wide. */
export function labelCollapseBaseMs(width: number): number {
  return Math.max(LABEL_MIN_MS, (Math.max(0, width) / LABEL_REF_WIDTH) * BASE_MS);
}

/**
 * Animate a label's horizontal slide between visible (natural width, opacity 1)
 * and collapsed (width 0, opacity 0). Width is measured live via `scrollWidth`
 * so dynamic content ("Downloads" -> "Downloading...") animates correctly.
 * `resolveDuration` turns the natural width-scaled ms into the app's effective
 * duration (client applies the animation settings; website returns it as-is, or
 * 0 to skip). Returning 0 collapses/expands instantly.
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
