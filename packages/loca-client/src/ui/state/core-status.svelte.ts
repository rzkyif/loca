/**
 * Backend core status (Starting Up / Idle / Busy / Updating / Error) for the
 * currently-selected core, fed by `core.status` WS frames. The core seeds it on
 * connect and broadcasts every change. Transport states (connecting /
 * reconnecting / disconnected / unauthorized) are NOT here: they live in
 * connectionState and are merged with this value in the CoreBar wrapper, since
 * a transport state can never ride the wire.
 */

import type { CoreStatusSnapshot, ServerToClientFrame } from "@loca/shared";
import { cores } from "$lib/core";
import { Subscriptions } from "$lib/util/subscriptions";

class CoreStatusState {
  snapshot = $state<CoreStatusSnapshot>({ status: "starting_up", subsystems: [] });

  private subs = new Subscriptions();
  private lastCoreId: string | null = null;

  attach(): void {
    this.subs.attach(() => {
      // On a core SWITCH (not a rename / pair / unpair of the same current
      // core), reset to starting_up until the newly-connected core's seed
      // frame arrives.
      this.lastCoreId = cores().currentEntry()?.id ?? null;
      return [
        cores().subscribeWs((frame: ServerToClientFrame) => {
          if (frame.kind === "core.status") this.snapshot = frame.snapshot;
        }),
        cores().subscribe(() => {
          const id = cores().currentEntry()?.id ?? null;
          if (id !== this.lastCoreId) {
            this.lastCoreId = id;
            this.snapshot = { status: "starting_up", subsystems: [] };
          }
        }),
      ];
    });
  }

  detach(): void {
    this.subs.detach();
    this.lastCoreId = null;
    this.snapshot = { status: "starting_up", subsystems: [] };
  }
}

export const coreStatusState = new CoreStatusState();
