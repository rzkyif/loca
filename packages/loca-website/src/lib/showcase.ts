import gsap from "gsap";

// Helpers for scripting a feature-showcase timeline: a virtual cursor that moves
// to, hovers, and clicks elements inside a stage, plus simulated typing and
// scrolling. Targets are resolved lazily by CSS selector (queried inside the
// stage at tween time) so elements that only appear mid-timeline still resolve.

export type Timeline = gsap.core.Timeline;
type Pos = number | string | undefined;

// App-surface dimensions, derived from the client's fixed 700x1200 window
// (tauri.conf.json) and rendered 1:1 in the demo.
//   APP_W: chat/settings content width = window 700 - the panel's max-w cap
//          (5rem) -> 620; also the bubble `max-w-[calc(100vw-5rem)]` value.
//   APP_H: the tallest surface, the settings panel at 80vh of 1200 = 960.
//   APP_SHADOW: outward reach of the bubble drop shadow + blur halo, so the box
//          leaves room for it instead of clipping the top/bottom edges.
export const APP_W = 620;
export const APP_H = 960;
export const APP_SHADOW = 20;

// The demo inputs show a blinking text caret while the cursor "types" into them.
// Stealing real focus is bad UX (and a focused readOnly field paints no caret in
// Chromium anyway), so the caret is a faux element the showcase positions itself
// at the end of the typed text. Skipped under reduced motion (a blink is motion).
function prefersReduced(): boolean {
  return typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
}

type EditableEl = HTMLInputElement | HTMLTextAreaElement;
// The `<input>` types that render typed text and so can carry a text caret; a
// range/checkbox/color/etc. input is an HTMLInputElement too but paints no text.
const TEXT_INPUT_TYPES = new Set(["text", "search", "url", "email", "tel", "password", "number"]);
function isEditable(el: HTMLElement): el is EditableEl {
  if (el instanceof HTMLTextAreaElement) return true;
  if (el instanceof HTMLInputElement) return TEXT_INPUT_TYPES.has(el.type);
  return false;
}

// (x, y, height) of the caret at the end of the text, in the EDITABLE's own
// UNSCALED local space (x/y from its border-box top-left; the caller maps that
// into the stage and applies the render scale).
type CaretLocal = { x: number; y: number; h: number };

// Caret position via the classic mirror-div technique: a throwaway div is given
// the editable's exact box model and font, filled with the text up to the caret
// plus a marker span, and the marker's box is read back. We read the marker with
// `getBoundingClientRect()` (an ELEMENT method, reliable even on a hidden node)
// rather than a Range over the live field's hidden mirror, whose
// `getClientRects()` returns nothing in WebKit. Works for wrapped textareas and
// single-line inputs alike, with no focus stolen. `boxW` is the field's LIVE
// fractional border-box width (its on-screen width divided back by the stage
// scale): the composer is shrink-to-fit, so an integer `offsetWidth` can be a
// hair narrower than the real text and wrap the last glyph, which made the caret
// jitter a line down frame to frame.
function caretLocal(el: EditableEl, boxW: number): CaretLocal | null {
  const isInput = el instanceof HTMLInputElement;
  const cs = getComputedStyle(el);
  const div = document.createElement("div");
  const st = div.style;
  st.position = "absolute";
  st.top = "0";
  st.left = "0";
  st.visibility = "hidden";
  st.pointerEvents = "none";
  st.overflow = "hidden";
  st.boxSizing = "border-box";
  st.width = `${boxW}px`;
  st.whiteSpace = isInput ? "nowrap" : "pre-wrap";
  st.wordWrap = "break-word";
  st.borderStyle = "solid"; // so the copied border widths shrink the content box
  st.direction = cs.direction;
  st.textAlign = cs.textAlign;
  st.textTransform = cs.textTransform;
  st.textIndent = cs.textIndent;
  st.letterSpacing = cs.letterSpacing;
  st.wordSpacing = cs.wordSpacing;
  st.fontStyle = cs.fontStyle;
  st.fontVariant = cs.fontVariant;
  st.fontWeight = cs.fontWeight;
  st.fontStretch = cs.fontStretch;
  st.fontSize = cs.fontSize;
  st.lineHeight = cs.lineHeight;
  st.fontFamily = cs.fontFamily;
  st.paddingTop = cs.paddingTop;
  st.paddingRight = cs.paddingRight;
  st.paddingBottom = cs.paddingBottom;
  st.paddingLeft = cs.paddingLeft;
  st.borderTopWidth = cs.borderTopWidth;
  st.borderRightWidth = cs.borderRightWidth;
  st.borderBottomWidth = cs.borderBottomWidth;
  st.borderLeftWidth = cs.borderLeftWidth;
  st.setProperty("tab-size", cs.tabSize);

  // Place the caret at the right edge of the LAST typed glyph (or the content-box
  // start for an empty field). The marker MUST be that last existing char, not an
  // appended one: the composer textarea is shrink-to-fit, so its width hugs the
  // text with no slack, and an extra glyph would wrap to a new line and throw the
  // caret a line down and to the far left. Inputs collapse whitespace on one line,
  // so feed them non-breaking spaces to keep the caret column honest.
  const v = el.value;
  const marker = document.createElement("span");
  let edge: "left" | "right";
  if (v.length === 0) {
    div.textContent = "";
    marker.textContent = "​"; // zero-width box at the start
    edge = "left";
  } else {
    let head = v.slice(0, -1);
    let last = v.slice(-1);
    if (isInput) {
      head = head.replace(/\s/g, " ");
      if (/\s/.test(last)) last = " ";
    }
    div.textContent = head;
    marker.textContent = last;
    edge = "right";
  }
  div.appendChild(marker);
  document.body.appendChild(div);
  div.scrollTop = el.scrollTop;
  div.scrollLeft = el.scrollLeft;

  const dRect = div.getBoundingClientRect();
  const mRect = marker.getBoundingClientRect();
  div.remove();
  // Line-box height from the measured marker (fall back to line-height). The
  // caret is a touch shorter than the line and vertically centered in it, so it
  // hugs the glyphs instead of spanning the full leading and looking too tall.
  const fontSize = parseFloat(cs.fontSize) || 16;
  let lineH = mRect.height;
  if (!Number.isFinite(lineH) || lineH <= 0) {
    lineH = parseFloat(cs.lineHeight);
    if (Number.isNaN(lineH)) lineH = fontSize * 1.2;
  }
  const caretH = Math.min(lineH, fontSize * 1.1);
  // Deltas between two unscaled element rects: the caret's offset from the
  // editable's border-box top-left, in unscaled px.
  return {
    x: (edge === "right" ? mRect.right : mRect.left) - dRect.left,
    y: mRect.top - dRect.top + (lineH - caretH) / 2,
    h: caretH,
  };
}

// A small firework spark: a pop ring that scales out plus a ring of slivers that
// shoot radially and fade. Built as throwaway DOM in the stage's UNSCALED space
// (so the ancestor stage transform scales it like the cursor), animated off the
// GSAP global timeline, and removed when done. A single high-contrast color
// matched to the RENDITION's theme, which is the inverse of the website's (the
// demo frame is theme-flipped): white burst on the dark rendition a light site
// shows, black on the light rendition a dark site shows.
function spawnBurst(stage: HTMLElement, x: number, y: number): void {
  const color = document.documentElement.classList.contains("dark") ? "#000000" : "#ffffff";
  const root = document.createElement("div");
  root.setAttribute("aria-hidden", "true");
  root.style.cssText =
    "position:absolute;left:0;top:0;z-index:40;pointer-events:none;will-change:transform;" +
    `transform:translate(${x}px,${y}px)`;

  const ring = document.createElement("div");
  ring.style.cssText =
    "position:absolute;left:-5px;top:-5px;width:10px;height:10px;border-radius:9999px;" +
    `border:1.5px solid ${color}`;
  root.appendChild(ring);

  const N = 8;
  const sparks: HTMLElement[] = [];
  for (let i = 0; i < N; i++) {
    const s = document.createElement("div");
    s.style.cssText =
      "position:absolute;left:-1px;top:-3px;width:2px;height:6px;border-radius:9999px;" +
      `background:${color}`;
    root.appendChild(s);
    sparks.push(s);
  }
  stage.appendChild(root);

  gsap.to(ring, { scale: 2.6, opacity: 0, duration: 0.45, ease: "power2.out" });
  sparks.forEach((s, i) => {
    const angle = (i / N) * Math.PI * 2 + (Math.random() - 0.5) * 0.4;
    const dist = 15 + Math.random() * 9;
    gsap.set(s, {
      rotation: (angle * 180) / Math.PI + 90,
      transformOrigin: "50% 50%",
    });
    gsap.to(s, {
      x: Math.cos(angle) * dist,
      y: Math.sin(angle) * dist,
      scale: 0.4,
      opacity: 0,
      duration: 0.5,
      ease: "power2.out",
    });
  });
  gsap.delayedCall(0.6, () => root.remove());
}

export class Demo {
  // The text field the cursor last clicked into. It becomes the caret's home once
  // typing starts, so the faux caret blinks at the typed text's end; it is dropped
  // the moment the cursor clicks elsewhere.
  private caretTarget?: EditableEl;
  private caretEl?: HTMLElement;

  constructor(
    private cursor: HTMLElement,
    private stage: HTMLElement,
  ) {}

  private q(sel: string): HTMLElement | null {
    return this.stage.querySelector<HTMLElement>(sel);
  }

  /** The stage's render scale: the showcase frame is sized at a fixed design
   *  resolution and CSS-`transform: scale()`d to fit. `getBoundingClientRect`
   *  returns scaled (on-screen) pixels while `offsetWidth` stays the unscaled
   *  layout width, so their ratio recovers the active scale. The cursor's
   *  translate lives in the stage's UNSCALED local space (the ancestor transform
   *  scales it), so every position below is divided back by this. */
  private scale(): number {
    const w = this.stage.getBoundingClientRect().width;
    const o = this.stage.offsetWidth;
    return o ? w / o : 1;
  }

  /** Center of `el` in the stage's unscaled local coordinate space. */
  private center(el: HTMLElement): { x: number; y: number } {
    const r = el.getBoundingClientRect();
    const s = this.stage.getBoundingClientRect();
    const k = this.scale();
    return {
      x: (r.left - s.left + r.width / 2) / k,
      y: (r.top - s.top + r.height / 2) / k,
    };
  }

  /** Place the cursor instantly at fractional stage coordinates (0..1). Uses the
   *  unscaled layout size so the fraction maps into the cursor's local space.
   *  Also reveals the cursor (it starts opacity-0 so the SSR render never shows
   *  it parked at the corner), so its first painted frame is already in place. */
  placeFrac(fx: number, fy: number): void {
    gsap.set(this.cursor, {
      x: this.stage.offsetWidth * fx,
      y: this.stage.offsetHeight * fy,
      opacity: 1,
    });
  }

  /** Tween the cursor to a target element (lazily resolved). */
  move(tl: Timeline, sel: string, opts: { duration?: number; at?: Pos } = {}): void {
    tl.to(
      this.cursor,
      {
        duration: opts.duration ?? 0.7,
        ease: "power2.inOut",
        x: () => {
          const el = this.q(sel);
          return el ? this.center(el).x : (gsap.getProperty(this.cursor, "x") as number);
        },
        y: () => {
          const el = this.q(sel);
          return el ? this.center(el).y : (gsap.getProperty(this.cursor, "y") as number);
        },
      },
      opts.at,
    );
  }

  /** Toggle the `data-hover` attribute (resolves to the same styles as :hover). */
  hover(tl: Timeline, sel: string, on: boolean, at?: Pos): void {
    tl.add(() => {
      const el = this.q(sel);
      if (!el) return;
      if (on) el.setAttribute("data-hover", "");
      else el.removeAttribute("data-hover");
    }, at);
  }

  /** A click: a brief cursor press + a firework spark + a flash of `data-active`,
   *  then `fn`. Clicking a text field records it as the caret's home (the caret
   *  only appears once `type` runs); clicking anything else drops that caret. */
  click(tl: Timeline, sel: string, fn?: () => void, at?: Pos): void {
    tl.to(
      this.cursor,
      {
        scale: 0.82,
        duration: 0.07,
        yoyo: true,
        repeat: 1,
        ease: "power1.inOut",
      },
      at,
    ).add(() => {
      const el = this.q(sel);
      if (el) {
        el.setAttribute("data-active", "");
        gsap.delayedCall(0.18, () => el.removeAttribute("data-active"));
      }
      // A firework-pop sparkle bloom under the cursor tip on every click.
      if (!prefersReduced()) {
        const x = gsap.getProperty(this.cursor, "x") as number;
        const y = gsap.getProperty(this.cursor, "y") as number;
        spawnBurst(this.stage, x + 3, y + 2);
      }
      // Caret: clicking into a text field marks it as the caret's home for the
      // next `type`; clicking anything else (e.g. Send, a slider) drops the caret.
      this.hideCaret();
      if (el && isEditable(el)) this.caretTarget = el;
      fn?.();
    });
  }

  /** Drop any caret the cursor placed in a text field (e.g. on stage reset). */
  blur(): void {
    this.hideCaret();
  }

  /** Reveal the faux caret in the last-clicked text field and pin it to the
   *  text's end. No-op unless the cursor clicked into a text field first. */
  private showCaret(): void {
    if (prefersReduced()) return;
    const el = this.caretTarget;
    if (!el) return;
    if (!this.caretEl) {
      const c = document.createElement("div");
      c.setAttribute("aria-hidden", "true");
      c.style.cssText =
        "position:absolute;left:0;top:0;z-index:45;width:2px;border-radius:1px;" +
        "pointer-events:none;will-change:transform,opacity";
      // A crisp on/off blink (no fade), like a real text caret.
      c.animate(
        [
          { opacity: 1, offset: 0 },
          { opacity: 1, offset: 0.5 },
          { opacity: 0, offset: 0.5 },
          { opacity: 0, offset: 1 },
        ],
        { duration: 1060, iterations: Infinity },
      );
      this.caretEl = c;
    }
    // Match the field's own resolved text color, so the caret is theme-aware
    // (dark on light, light on dark) with no token guessing.
    this.caretEl.style.background = getComputedStyle(el).color;
    if (this.caretEl.parentElement !== this.stage) {
      this.stage.appendChild(this.caretEl);
    }
    this.caretEl.style.display = "";
    this.positionCaret();
  }

  private hideCaret(): void {
    this.caretTarget = undefined;
    if (this.caretEl) this.caretEl.style.display = "none";
  }

  /** Move the caret to the current text end, in the stage's unscaled local space. */
  private positionCaret(): void {
    if (!this.caretTarget || !this.caretEl) return;
    // The editable's border-box origin and LIVE fractional size in the stage's
    // UNSCALED local space (its on-screen rect divided back by the render scale).
    const el = this.caretTarget.getBoundingClientRect();
    const s = this.stage.getBoundingClientRect();
    const k = this.scale();
    const local = caretLocal(this.caretTarget, el.width / k);
    if (!local) return;
    const x = (el.left - s.left) / k + local.x;
    const y = (el.top - s.top) / k + local.y;
    this.caretEl.style.height = `${local.h}px`;
    this.caretEl.style.transform = `translate(${x}px, ${y}px)`;
  }

  /** Type `text` character by character into a setter (e.g. a `$state` writer). */
  type(
    tl: Timeline,
    set: (v: string) => void,
    text: string,
    opts: { duration?: number; at?: Pos } = {},
  ): void {
    const o = { n: 0 };
    tl.to(
      o,
      {
        n: text.length,
        duration: opts.duration ?? text.length * 0.055,
        ease: "none",
        snap: { n: 1 },
        // Reveal the caret only now that typing is actually happening (and only
        // if the cursor clicked into a text field first).
        onStart: () => this.showCaret(),
        onUpdate: () => {
          set(text.slice(0, Math.round(o.n)));
          // Reposition after Svelte flushes the new value into the mirror (next
          // microtask), so the caret tracks the rendered text end with no lag.
          queueMicrotask(() => this.positionCaret());
        },
      },
      opts.at,
    );
  }

  /** A still hold of `dur` seconds. */
  hold(tl: Timeline, dur: number, at?: Pos): void {
    tl.to({}, { duration: dur }, at);
  }

  /** Smoothly scroll an element's `scrollTop` from its current value to `target`.
   *  `target` may be a function resolved at tween start, so a stage can scroll to
   *  wherever a (lazily measured) element currently sits. */
  scroll(
    tl: Timeline,
    getEl: () => HTMLElement | undefined,
    target: number | (() => number),
    opts: { duration?: number; at?: Pos } = {},
  ): void {
    const o = { v: 0 };
    let from = 0;
    let to = 0;
    tl.to(
      o,
      {
        v: 1,
        duration: opts.duration ?? 1.6,
        ease: "power1.inOut",
        onStart: () => {
          from = getEl()?.scrollTop ?? 0;
          to = typeof target === "function" ? target() : target;
        },
        onUpdate: () => {
          const el = getEl();
          if (el) el.scrollTop = from + (to - from) * o.v;
        },
      },
      opts.at,
    );
  }

  /** Scroll a container so `sel` sits `pad` px below its top edge. Returns the
   *  target scrollTop (for `scroll`), 0 if the element or container is missing. */
  scrollSelectorTop(getEl: () => HTMLElement | undefined, sel: string, pad = 72): number {
    const sc = getEl();
    const el = this.q(sel);
    if (!sc || !el) return sc?.scrollTop ?? 0;
    const er = el.getBoundingClientRect();
    const sr = sc.getBoundingClientRect();
    return sc.scrollTop + (er.top - sr.top) - pad;
  }
}

/** Resolve once `el`'s height has been stable for a few animation frames (so an
 *  in-flight expand / font swap has settled), or after `maxFrames`. Used by the
 *  measured chat stages so the reported frame height never under-measures and
 *  clips the column. */
export function awaitStableHeight(el: HTMLElement, maxFrames = 48): Promise<number> {
  return new Promise((resolve) => {
    let last = -1;
    let stable = 0;
    let frames = 0;
    const tick = (): void => {
      const h = el.offsetHeight;
      if (h === last) stable++;
      else {
        stable = 0;
        last = h;
      }
      frames++;
      if (stable >= 3 || frames >= maxFrames) resolve(last);
      else requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}
