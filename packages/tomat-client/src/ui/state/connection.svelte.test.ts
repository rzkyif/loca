// The connection store's job is to turn CoreClient's transport states into the
// two things the UI reads: which pill the CoreBar paints, and the single
// `coreUnavailable` flag every core-backed control disables itself on. The
// invariant under test is that a brief blip never trips that flag (the 5s
// grace), while every settled failure trips it at once.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ConnectionListener } from "$lib/core/client";

// Capture the listener the store registers, so tests can drive transitions
// without a registry, a platform, or a socket.
let listener: ConnectionListener | null = null;
vi.mock("$lib/core", () => ({
  cores: () => ({
    subscribeConnectionState: (l: ConnectionListener) => {
      listener = l;
      return () => {
        listener = null;
      };
    },
  }),
}));

const { connectionState } = await import("./connection.svelte");

function emit(...args: Parameters<ConnectionListener>): void {
  if (!listener) throw new Error("store is not attached");
  listener(...args);
}

describe("connectionState", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    connectionState.attach();
  });
  afterEach(() => {
    connectionState.detach();
    vi.useRealTimers();
  });

  it("holds coreUnavailable back for the 5s grace, then trips it", () => {
    emit("disconnected", "Connection refused");
    // A blip that resolves inside the grace must not disable half the UI.
    expect(connectionState.coreUnavailable).toBe(false);
    vi.advanceTimersByTime(4_999);
    expect(connectionState.coreUnavailable).toBe(false);
    vi.advanceTimersByTime(1);
    expect(connectionState.reconnecting).toBe(true);
    expect(connectionState.coreUnavailable).toBe(true);
    expect(connectionState.unavailableCopy).toBe("Reconnecting to Core...");
  });

  it("a reconnect inside the grace leaves nothing disabled", () => {
    emit("disconnected");
    vi.advanceTimersByTime(3_000);
    emit("connected");
    vi.advanceTimersByTime(10_000);
    expect(connectionState.reconnecting).toBe(false);
    expect(connectionState.coreUnavailable).toBe(false);
    expect(connectionState.unavailableCopy).toBeNull();
  });

  // The parked states are already settled, so they need no grace: the transport
  // has stopped retrying and the user has to act.
  it.each([
    ["gaveUp", "Disconnected from Core..."],
    ["stopped", "Core is stopped..."],
    ["unauthorized", "Re-pair needed..."],
  ] as const)("parks immediately on %s", (state, copy) => {
    emit(state, "why");
    expect(connectionState.coreUnavailable).toBe(true);
    expect(connectionState.reconnecting).toBe(false);
    expect(connectionState.unavailableCopy).toBe(copy);
    expect(connectionState.reason).toBe("why");
  });

  it("sets exactly one parked flag at a time", () => {
    emit("stopped");
    expect([connectionState.stopped, connectionState.gaveUp, connectionState.unauthorized]).toEqual(
      [true, false, false],
    );
    // A parked client only re-enters "connecting" via a retry or a core switch;
    // either way the stale flag must clear or a healthy core reads as stopped.
    emit("connecting");
    expect(connectionState.stopped).toBe(false);
    expect(connectionState.coreUnavailable).toBe(false);
  });

  it("connected clears every failure flag", () => {
    emit("gaveUp", "refused");
    emit("connected");
    expect(connectionState.coreUnavailable).toBe(false);
    expect(connectionState.gaveUp).toBe(false);
    expect(connectionState.reason).toBeNull();
  });

  // Clicking the pill has to read as a live retry at once; waiting out the 5s
  // grace before the label changes would read as a dead click.
  it("retryFromUi shows Reconnecting immediately and drops the parked flags", () => {
    emit("gaveUp", "refused");
    connectionState.retryFromUi();
    expect(connectionState.gaveUp).toBe(false);
    expect(connectionState.stopped).toBe(false);
    expect(connectionState.reconnecting).toBe(true);
    expect(connectionState.unavailableCopy).toBe("Reconnecting to Core...");

    // The transport's own "connecting" must not re-arm the grace timer and
    // silently drop the spinner back to "Connecting".
    emit("connecting");
    vi.advanceTimersByTime(10_000);
    expect(connectionState.reconnecting).toBe(true);
  });
});
