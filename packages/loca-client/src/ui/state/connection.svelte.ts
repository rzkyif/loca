/**
 * Reactive WebSocket connection state for the currently-selected core, driving
 * the adaptive core-unavailable UI (chat input, settings lock-down, session-list
 * button, quick-settings redirect). Hides flashes of disconnection that resolve
 * within RECONNECT_BANNER_DELAY_MS. The underlying CoreClient retries with exp
 * backoff (500 ms → 30 s), so a brief reconnect during normal operation
 * shouldn't be visible; after six failed attempts it parks and we show
 * "Disconnected" (or "Stopped", when this device's core simply isn't running).
 */

import type { ConnectionState } from "$lib/core/client";
import { cores } from "$lib/core";
import { Subscriptions } from "$lib/util/subscriptions";

const RECONNECT_BANNER_DELAY_MS = 5_000;

class ConnectionStateStore {
  state = $state<ConnectionState>("connecting");
  disconnectedSinceMs = $state<number | null>(null);

  // True once disconnected for at least RECONNECT_BANNER_DELAY_MS. Kept
  // as plain state (not $derived) so we can manage the delay timer. Drives the
  // adaptive reconnecting UI across chat / settings / quick-settings modes.
  reconnecting = $state(false);

  // True when the core rejected our bearer token (its DB was reset, this
  // client was revoked, ...). Terminal: the underlying client stops retrying,
  // so the UI shows a re-pair prompt rather than a "reconnecting" spinner.
  unauthorized = $state(false);

  // True once the client gave up reconnecting (six consecutive failed attempts).
  // Terminal until the user clicks the pill, which calls retryFromUi().
  gaveUp = $state(false);

  // True when this device's core process is not running. Terminal until the user
  // starts it. Distinct from gaveUp because the fix is different: start the core,
  // rather than retry a connection that could never have landed.
  stopped = $state(false);

  // Last connect-failure reason from CoreClient (e.g. "Connection refused"),
  // shown in the banner. Retained across the connecting/disconnected churn of
  // the reconnect loop; cleared once connected.
  reason = $state<string | null>(null);

  /** The single guard the UI reads: the core cannot serve a request right now,
   *  so every control that would need one is inert. Deliberately keyed on the
   *  5s-graced `reconnecting` rather than the raw `disconnected` state, so a
   *  brief blip during normal operation doesn't flicker the whole UI. The other
   *  three are already terminal, so they need no grace period. */
  get coreUnavailable(): boolean {
    return this.reconnecting || this.gaveUp || this.stopped || this.unauthorized;
  }

  /** Why the core is unavailable, as a placeholder for the controls that turn
   *  inert (the composer, the settings search). Null while it is reachable.
   *  Shared so the two surfaces can't word the same state differently. */
  get unavailableCopy(): string | null {
    if (this.unauthorized) return "Re-pair needed...";
    if (this.stopped) return "Core is stopped...";
    if (this.gaveUp) return "Disconnected from Core...";
    if (this.reconnecting) return "Reconnecting to Core...";
    return null;
  }

  /** The user asked to reconnect (clicked the Disconnected / Stopped pill).
   *  Shows "Reconnecting" at once rather than waiting out the 5s grace on the
   *  transport's next `connecting`, which would read as a dead click. The caller
   *  drives the transport itself via `CoreClient.retryNow()`. */
  retryFromUi(): void {
    this.clearBannerTimer();
    this.gaveUp = false;
    this.stopped = false;
    this.reconnecting = true;
  }

  private subs = new Subscriptions();
  // ReturnType so the type works under both browser (number) and node typing.
  private bannerTimer: ReturnType<typeof setTimeout> | null = null;

  attach(): void {
    this.subs.attach(() => [
      cores().subscribeConnectionState((s, r) => {
        this.state = s;
        if (s === "connected") {
          this.disconnectedSinceMs = null;
          this.clearBannerTimer();
          this.reconnecting = false;
          this.unauthorized = false;
          this.gaveUp = false;
          this.stopped = false;
          this.reason = null;
        } else if (s === "unauthorized" || s === "gaveUp" || s === "stopped") {
          // Terminal: the client has stopped retrying. Drop the reconnect spinner
          // and show the state's own affordance; keep the reason for the message.
          this.clearBannerTimer();
          this.reconnecting = false;
          this.unauthorized = s === "unauthorized";
          this.gaveUp = s === "gaveUp";
          this.stopped = s === "stopped";
          if (r) this.reason = r;
        } else if (s === "disconnected" || s === "connecting") {
          // The client never leaves a parked state for the SAME core on its own,
          // so a fresh connecting/disconnected here means either the user asked
          // to retry or we're now tracking a different/re-paired core. Either
          // way, drop the stale parked flags, or a healthy core would keep
          // showing "Re-pair needed" / "Disconnected" forever.
          if (this.unauthorized || this.gaveUp || this.stopped) {
            this.unauthorized = false;
            this.gaveUp = false;
            this.stopped = false;
            this.reason = null;
          }
          // Keep the last known reason through the connecting/disconnected churn.
          if (r) this.reason = r;
          if (this.disconnectedSinceMs === null) {
            this.disconnectedSinceMs = Date.now();
          }
          if (!this.reconnecting && this.bannerTimer === null) {
            this.bannerTimer = setTimeout(() => {
              this.bannerTimer = null;
              // Re-check at fire time: a quick reconnect may have already
              // flipped state back.
              if (this.state !== "connected") this.reconnecting = true;
            }, RECONNECT_BANNER_DELAY_MS);
          }
        }
      }),
    ]);
  }

  detach(): void {
    this.subs.detach();
    this.clearBannerTimer();
    this.state = "connecting";
    this.disconnectedSinceMs = null;
    this.reconnecting = false;
    this.unauthorized = false;
    this.gaveUp = false;
    this.stopped = false;
    this.reason = null;
  }

  private clearBannerTimer(): void {
    if (this.bannerTimer !== null) {
      clearTimeout(this.bannerTimer);
      this.bannerTimer = null;
    }
  }
}

export const connectionState = new ConnectionStateStore();
