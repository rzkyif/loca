// Exercises the shared `use:ripple` action's guard logic and lifecycle. The
// action lives in @loca/shared, but it is DOM-driven (getBoundingClientRect,
// element.animate, MutationObserver), so it is tested here in the client's
// jsdom/vitest environment (the same place shared primitives are covered).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ripple } from "@loca/shared/ui/actions/ripple.ts";

const LAYER = ".loca-ripple-layer";

function mountedNode(): HTMLElement {
  const node = document.createElement("button");
  // jsdom reports a 0x0 box; give the action a real size to splash into.
  node.getBoundingClientRect = () =>
    ({
      width: 100,
      height: 40,
      left: 0,
      top: 0,
      right: 100,
      bottom: 40,
      x: 0,
      y: 0,
      toJSON() {},
    }) as DOMRect;
  document.body.appendChild(node);
  return node;
}

function press(node: HTMLElement): void {
  node.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, clientX: 50, clientY: 20 }));
}

describe("ripple action", () => {
  beforeEach(() => {
    // jsdom has no WAAPI; provide a controllable stub so the splash dot persists
    // until we resolve it (mirrors how a real ripple is removed onfinish).
    (Element.prototype as unknown as { animate: unknown }).animate = vi.fn(() => ({
      onfinish: null as null | (() => void),
      oncancel: null as null | (() => void),
      cancel() {},
    }));
  });

  afterEach(() => {
    document.body.innerHTML = "";
    vi.restoreAllMocks();
  });

  it("spawns a clipped layer + splash on pointerdown", () => {
    const node = mountedNode();
    const handle = ripple(node, { durationMs: 400 });

    press(node);

    const layer = node.querySelector(LAYER);
    expect(layer).not.toBeNull();
    expect(layer!.childElementCount).toBe(1);
    expect(Element.prototype.animate).toHaveBeenCalledTimes(1);
    // The host is made a positioning context for the absolute layer.
    expect(node.style.position).toBe("relative");

    handle?.destroy?.();
    expect(node.querySelector(LAYER)).toBeNull();
  });

  it("renders nothing when disabled", () => {
    const node = mountedNode();
    const handle = ripple(node, { disabled: true, durationMs: 400 });

    press(node);

    expect(node.querySelector(LAYER)).toBeNull();
    expect(Element.prototype.animate).not.toHaveBeenCalled();
    handle?.destroy?.();
  });

  it("renders nothing when the resolved duration is zero (reduced motion)", () => {
    const node = mountedNode();
    const handle = ripple(node, { durationMs: 0 });

    press(node);

    expect(node.querySelector(LAYER)).toBeNull();
    handle?.destroy?.();
  });

  it("mirrors the demo cursor's data-active press", async () => {
    const node = mountedNode();
    const handle = ripple(node, { durationMs: 400 });

    node.setAttribute("data-active", "");
    // MutationObserver callbacks are microtask-scheduled.
    await Promise.resolve();
    await Promise.resolve();

    expect(node.querySelector(LAYER)?.childElementCount).toBe(1);
    handle?.destroy?.();
  });
});
