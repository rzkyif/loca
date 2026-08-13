// Touch long-press detection. Fires the callback when a TOUCH pointer is held
// still for ~`delay`ms, the gesture that stands in for a right-click on the
// mobile shell (open an action sheet for a message, a session, ...). Gated to
// `pointerType === "touch"` so it never interferes with desktop mouse input,
// where the native context menu still handles right-click. No-op when no
// callback is supplied, so a component can attach it unconditionally.

import type { Action } from "svelte/action";

const DEFAULT_DELAY_MS = 450;
const MOVE_CANCEL_PX = 10;

export const longpress: Action<HTMLElement, (() => void) | undefined> = (node, callback) => {
  let cb = callback;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let startX = 0;
  let startY = 0;
  let fired = false;

  const clear = (): void => {
    if (timer !== undefined) {
      clearTimeout(timer);
      timer = undefined;
    }
  };

  const onPointerDown = (e: PointerEvent): void => {
    if (!cb || e.pointerType !== "touch") return;
    fired = false;
    startX = e.clientX;
    startY = e.clientY;
    clear();
    timer = setTimeout(() => {
      fired = true;
      cb?.();
    }, DEFAULT_DELAY_MS);
  };

  const onPointerMove = (e: PointerEvent): void => {
    if (timer === undefined) return;
    if (
      Math.abs(e.clientX - startX) > MOVE_CANCEL_PX ||
      Math.abs(e.clientY - startY) > MOVE_CANCEL_PX
    ) {
      clear();
    }
  };

  const onPointerEnd = (): void => clear();

  // Swallow the click that follows a fired long-press so it does not also
  // trigger the element's tap handler (e.g. opening the session you held).
  const onClick = (e: MouseEvent): void => {
    if (fired) {
      e.stopPropagation();
      e.preventDefault();
      fired = false;
    }
  };

  node.addEventListener("pointerdown", onPointerDown);
  node.addEventListener("pointermove", onPointerMove);
  node.addEventListener("pointerup", onPointerEnd);
  node.addEventListener("pointercancel", onPointerEnd);
  node.addEventListener("pointerleave", onPointerEnd);
  node.addEventListener("click", onClick, true);

  return {
    update(next) {
      cb = next;
    },
    destroy() {
      clear();
      node.removeEventListener("pointerdown", onPointerDown);
      node.removeEventListener("pointermove", onPointerMove);
      node.removeEventListener("pointerup", onPointerEnd);
      node.removeEventListener("pointercancel", onPointerEnd);
      node.removeEventListener("pointerleave", onPointerEnd);
      node.removeEventListener("click", onClick, true);
    },
  };
};
