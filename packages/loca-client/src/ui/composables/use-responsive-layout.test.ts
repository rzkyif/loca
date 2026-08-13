// ResponsiveLayout: verifies the derived `horizontal` flag tracks the
// reactive `containerWidth` against the threshold. ResizeObserver wiring
// is exercised by the running app and the stubbed RO in test/setup.

import { describe, expect, it } from "vitest";
import { flushSync } from "svelte";
import { ResponsiveLayout, useResponsiveLayout } from "./use-responsive-layout.svelte";

describe("ResponsiveLayout", () => {
  it("default threshold is 680; horizontal flips at the boundary", () => {
    const r = new ResponsiveLayout();
    flushSync();
    expect(r.threshold).toBe(680);
    expect(r.horizontal).toBe(false);

    r.containerWidth = 679;
    flushSync();
    expect(r.horizontal).toBe(false);

    r.containerWidth = 680;
    flushSync();
    expect(r.horizontal).toBe(true);

    r.containerWidth = 1024;
    flushSync();
    expect(r.horizontal).toBe(true);

    r.containerWidth = 200;
    flushSync();
    expect(r.horizontal).toBe(false);
  });

  it("useResponsiveLayout(threshold) creates an instance with the given threshold", () => {
    const r = useResponsiveLayout(900);
    r.containerWidth = 800;
    flushSync();
    expect(r.horizontal).toBe(false);
    r.containerWidth = 901;
    flushSync();
    expect(r.horizontal).toBe(true);
  });

  it("observe() returns a no-op cleanup when no containerEl is bound", () => {
    const r = new ResponsiveLayout();
    const cleanup = r.observe();
    expect(typeof cleanup).toBe("function");
    cleanup(); // should not throw
  });
});
