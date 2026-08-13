// The app-wide press affordance: a Material-style ink ripple that expands from
// the press point and fades. It is the single source of "this was pressed"
// feedback on every clickable, replacing the old `act:` press-color shift, so a
// tap feels native on touch and the desktop press gains a tactile splash. The
// `hov:` hover-color shift stays (desktop only, gated by `@media (hover: hover)`
// in the UnoCSS preset); the ripple covers the press half on every pointer.
//
// Modeled on `longpress.ts`: a plain Svelte action, listeners attached on mount
// and torn down on destroy. The splash is driven by the Web Animations API
// (transform + opacity only, so it stays on the compositor) inside a clip
// layer, so it never needs the host itself to set `overflow: hidden` (which
// would clip an IconButton's overflowing badge).

import type { Action } from "svelte/action";
import { CSS_EASING, RIPPLE_MS } from "../animations.ts";

export interface RippleOptions {
  /** Skip the ripple entirely (e.g. a disabled control). */
  disabled?: boolean;
  /** Resolved splash duration in ms. Callers pass
   *  `ui.animationDurationMs(RIPPLE_MS)` so the app's animation-speed setting
   *  and reduced-motion apply; `<= 0` renders no ripple. Omitted falls back to
   *  `RIPPLE_MS` gated by the OS `prefers-reduced-motion` query. */
  durationMs?: number;
  /** Splash color. Defaults to `currentColor` so it themes with the control's
   *  text and reads on both light and inverted surfaces. */
  color?: string;
}

const LAYER_CLASS = "loca-ripple-layer";
const PEAK_OPACITY = 0.22;

function prefersReducedMotion(): boolean {
  return typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
}

export const ripple: Action<HTMLElement, RippleOptions | undefined> = (node, options) => {
  let opts = options ?? {};

  // A dedicated clip layer covering the host's padding box. Ripples live inside
  // it so `overflow: hidden` confines them to the host's rounded shape without
  // clipping the host's own overflowing children (badges, focus rings).
  let layer: HTMLSpanElement | undefined;
  const ensureLayer = (): HTMLSpanElement => {
    if (layer) return layer;
    // The absolutely-positioned layer needs a positioned host to anchor to.
    if (getComputedStyle(node).position === "static") node.style.position = "relative";
    const el = document.createElement("span");
    el.className = LAYER_CLASS;
    el.setAttribute("aria-hidden", "true");
    el.style.cssText =
      "position:absolute;inset:0;overflow:hidden;border-radius:inherit;pointer-events:none;";
    node.appendChild(el);
    layer = el;
    return el;
  };

  const resolvedDuration = (): number => {
    if (opts.disabled) return 0;
    if (opts.durationMs !== undefined) return opts.durationMs;
    return prefersReducedMotion() ? 0 : RIPPLE_MS;
  };

  // Spawn one ripple centered at (clientX, clientY); falls back to the host
  // center when no point is given (keyboard activation, the demo cursor).
  const spawn = (clientX?: number, clientY?: number): void => {
    const duration = resolvedDuration();
    if (duration <= 0) return;
    const rect = node.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return;
    const x = clientX === undefined ? rect.width / 2 : clientX - rect.left;
    const y = clientY === undefined ? rect.height / 2 : clientY - rect.top;
    // Diameter that reaches the farthest corner from the press point, so the
    // splash always fills the control.
    const diameter =
      2 *
      Math.max(
        Math.hypot(x, y),
        Math.hypot(rect.width - x, y),
        Math.hypot(x, rect.height - y),
        Math.hypot(rect.width - x, rect.height - y),
      );

    const dot = document.createElement("span");
    dot.style.cssText =
      `position:absolute;left:${x - diameter / 2}px;top:${y - diameter / 2}px;` +
      `width:${diameter}px;height:${diameter}px;border-radius:9999px;` +
      `background:${opts.color ?? "currentColor"};opacity:${PEAK_OPACITY};` +
      `pointer-events:none;will-change:transform,opacity;transform:scale(0);`;
    ensureLayer().appendChild(dot);

    if (typeof dot.animate !== "function") {
      dot.remove();
      return;
    }
    const anim = dot.animate(
      [
        { transform: "scale(0)", opacity: PEAK_OPACITY },
        { transform: "scale(1)", opacity: 0 },
      ],
      { duration, easing: CSS_EASING, fill: "forwards" },
    );
    anim.onfinish = () => dot.remove();
    anim.oncancel = () => dot.remove();
  };

  const onPointerDown = (e: PointerEvent): void => spawn(e.clientX, e.clientY);

  // Keyboard activation (Enter / Space on a focused button) has no pointer
  // coordinates, so ripple from the center to keep the press legible.
  const onKeyDown = (e: KeyboardEvent): void => {
    if (e.repeat) return;
    if (e.key === "Enter" || e.key === " " || e.key === "Spacebar") spawn();
  };

  // Demo-cursor parity: the website's scripted cursor presses by toggling the
  // `data-active` attribute (the same attribute `act:` reads). Mirror it so a
  // scripted demo and a real tap render the identical splash, satisfying the
  // single-source rule.
  const observer =
    typeof MutationObserver === "function"
      ? new MutationObserver(() => {
          if (node.hasAttribute("data-active")) spawn();
        })
      : undefined;

  node.addEventListener("pointerdown", onPointerDown);
  node.addEventListener("keydown", onKeyDown);
  observer?.observe(node, { attributes: true, attributeFilter: ["data-active"] });

  return {
    update(next) {
      opts = next ?? {};
    },
    destroy() {
      node.removeEventListener("pointerdown", onPointerDown);
      node.removeEventListener("keydown", onKeyDown);
      observer?.disconnect();
      layer?.remove();
    },
  };
};
