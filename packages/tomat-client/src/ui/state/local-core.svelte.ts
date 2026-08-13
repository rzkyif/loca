/**
 * The core installed on THIS device: is it there, and is it running?
 *
 * Two consumers need this, and neither is the connection store: the tray's
 * Start / Stop Core item (which is about the local core regardless of which core
 * is currently selected, or whether any is paired at all), and the CoreBar's
 * Stopped pill. Both perform the same start / stop here, so the tray and the
 * pill can never drift.
 *
 * Nothing polls. The state is refreshed on boot, whenever the transport's
 * connection state changes, and after each start / stop, then pushed to the tray.
 */

import { cores } from "$lib/core";
import { platform } from "$lib/platform";
import { getLogger } from "$lib/util/log";
import { Subscriptions } from "$lib/util/subscriptions";
import { connectionState } from "./connection.svelte";

const log = getLogger("local-core");

class LocalCoreStore {
  /** Whether a core binary is present on this device (planted by the installer). */
  installed = $state(false);
  /** Whether that core's port is answering. Meaningless while `installed` is false. */
  running = $state(false);
  /** A start / stop is in flight; the UI disables its affordances meanwhile. */
  busy = $state(false);

  private subs = new Subscriptions();

  attach(): void {
    this.subs.attach(() => {
      const offs: Array<() => void> = [];
      // Re-probe on every transport transition: a disconnect is exactly when the
      // core's liveness may have changed, and a reconnect confirms it is back.
      offs.push(cores().subscribeConnectionState(() => void this.refresh()));
      let offTray: (() => void) | null = null;
      void platform()
        .tray.subscribeCoreAction((action) => {
          void (action === "start" ? this.start() : this.stop());
        })
        .then((off) => {
          offTray = off;
        });
      offs.push(() => offTray?.());
      void this.refresh();
      return offs;
    });
  }

  detach(): void {
    this.subs.detach();
  }

  /** Re-read both facts from the host and mirror them into the tray menu. */
  async refresh(): Promise<void> {
    try {
      const installed = await platform().pairing.isLocalCoreInstalled();
      const running = installed ? await platform().pairing.isLocalCoreRunning() : false;
      this.installed = installed;
      this.running = running;
      await platform().tray.setCoreState(installed, running);
    } catch (e) {
      log.error("local-core refresh failed:", e);
    }
  }

  /** Start the local core, then reconnect the selected core to it. Safe to call
   *  when it is already running (the host probes first). */
  async start(): Promise<void> {
    await this.run(() => platform().pairing.startLocalCore());
    connectionState.retryFromUi();
    cores().currentClient()?.retryNow();
  }

  /** Stop the local core, through its supervisor when it has one. */
  async stop(): Promise<void> {
    await this.run(() => platform().pairing.stopLocalCore());
  }

  private async run(action: () => Promise<unknown>): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    try {
      await action();
    } catch (e) {
      log.error("local-core action failed:", e);
    } finally {
      this.busy = false;
      await this.refresh();
    }
  }
}

export const localCoreState = new LocalCoreStore();
