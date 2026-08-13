// Blink: verifies the interval toggles `on` while active and that run()'s
// cleanup clears the timer. The consumer-owned $effect wiring is exercised by
// the running app; here we drive run() directly with fake timers.

import { afterEach, describe, expect, it, vi } from "vitest";
import { Blink, useBlink } from "./use-blink.svelte";

describe("Blink", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("starts false and run(false) returns a no-op cleanup", () => {
    vi.useFakeTimers();
    const b = new Blink();
    expect(b.on).toBe(false);

    const cleanup = b.run(false);
    expect(typeof cleanup).toBe("function");
    vi.advanceTimersByTime(2000);
    expect(b.on).toBe(false);
    cleanup(); // should not throw
  });

  it("toggles `on` on the interval while active", () => {
    vi.useFakeTimers();
    const b = useBlink(500);

    const cleanup = b.run(true);
    expect(b.on).toBe(false);
    vi.advanceTimersByTime(500);
    expect(b.on).toBe(true);
    vi.advanceTimersByTime(500);
    expect(b.on).toBe(false);
    cleanup();
  });

  it("cleanup() stops the interval; run(false) resets `on`", () => {
    vi.useFakeTimers();
    const b = new Blink(500);

    const cleanup = b.run(true);
    vi.advanceTimersByTime(500);
    expect(b.on).toBe(true);

    cleanup();
    vi.advanceTimersByTime(2000);
    expect(b.on).toBe(true); // no more toggling after cleanup

    b.run(false);
    expect(b.on).toBe(false);
  });

  it("useBlink(intervalMs) honors a custom interval", () => {
    vi.useFakeTimers();
    const b = useBlink(1000);

    const cleanup = b.run(true);
    vi.advanceTimersByTime(500);
    expect(b.on).toBe(false);
    vi.advanceTimersByTime(500);
    expect(b.on).toBe(true);
    cleanup();
  });
});
