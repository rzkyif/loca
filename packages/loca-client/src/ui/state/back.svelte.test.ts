// Exercises the back-handler registry's priority chain: interceptors win in
// LIFO order, a non-chat mode returns to chat, and the chat root needs a
// double-back to exit. The platform stub backs platform().backButton.exit.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installPlatformStub } from "../test/platform-stub.ts";
import { platform } from "$lib/platform";
import { backState } from "./back.svelte";
import { viewState } from "./view.svelte";

describe("backState", () => {
  let disposers: Array<() => void> = [];
  let exitSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    vi.useFakeTimers();
    installPlatformStub();
    exitSpy = vi.spyOn(platform().backButton, "exit");
    // Start every case at a settled chat root with a paired core.
    viewState.setImmediate("chat");
    viewState.setLocked(false);
  });

  afterEach(() => {
    disposers.forEach((d) => d());
    disposers = [];
    // Flush any armed exit window so it never bleeds into the next test.
    vi.runOnlyPendingTimers();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  function push(handler: () => boolean): void {
    disposers.push(backState.push(handler));
  }

  it("routes a press to the topmost interceptor (LIFO)", () => {
    const order: string[] = [];
    push(() => {
      order.push("first");
      return false; // declines, so the press falls through to the next
    });
    push(() => {
      order.push("second");
      return true; // consumes
    });

    backState.back();

    expect(order).toEqual(["second"]);
    expect(viewState.pendingMode).toBe("chat");
  });

  it("falls through a declining interceptor to mode navigation", () => {
    viewState.setImmediate("settings");
    push(() => false);

    backState.back();

    expect(viewState.pendingMode).toBe("chat");
  });

  it("returns a non-chat mode to chat before exiting", () => {
    viewState.setImmediate("sessionList");

    backState.back();

    expect(viewState.pendingMode).toBe("chat");
    expect(exitSpy).not.toHaveBeenCalled();
  });

  it("needs a double-back to exit from the chat root", () => {
    backState.back();
    expect(backState.exitHint).toBe(true);
    expect(exitSpy).not.toHaveBeenCalled();

    backState.back();
    expect(exitSpy).toHaveBeenCalledTimes(1);
  });

  it("disarms the exit hint after the window elapses", () => {
    backState.back();
    expect(backState.exitHint).toBe(true);

    vi.advanceTimersByTime(2000);
    expect(backState.exitHint).toBe(false);

    // A lone back after the window only re-arms; it does not exit.
    backState.back();
    expect(exitSpy).not.toHaveBeenCalled();
  });

  it("removes a popped handler", () => {
    let calls = 0;
    const dispose = backState.push(() => {
      calls++;
      return true;
    });
    dispose();

    viewState.setImmediate("settings");
    backState.back();

    expect(calls).toBe(0);
    expect(viewState.pendingMode).toBe("chat");
  });
});
