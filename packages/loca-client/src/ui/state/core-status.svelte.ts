// Per-core state that must reset on a core SWITCH (both attach from
// +layout.svelte): the backend status fed by `core.status` WS frames, and what
// the core can do, read from `GET /api/v1/health` on connect. Transport states
// (connecting / reconnecting / disconnected / unauthorized) live in
// connectionState and are merged with the status in the CoreBar wrapper.

import type {
  CoreCapabilities,
  CoreStatusSnapshot,
  HealthResponse,
  ServerToClientFrame,
} from "@loca/shared";
import { cores } from "$lib/core";
import { getLogger } from "$lib/util/log";
import { Subscriptions } from "$lib/util/subscriptions";

const log = getLogger("capabilities");

class CoreStatusState {
  snapshot = $state<CoreStatusSnapshot>({ status: "starting_up", subsystems: [] });

  private subs = new Subscriptions();
  private lastCoreId: string | null = null;

  attach(): void {
    this.subs.attach(() => {
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

// Capability is a property of the CORE, not the device rendering this UI: a
// phone paired to a desktop core must still offer extensions, scheduled prompts
// and stdio MCP servers. Device concerns stay on `ui.platform` / `desktopOnly`.
class CapabilitiesState {
  /** Null until the first health response for this core lands. */
  current = $state<CoreCapabilities | null>(null);

  private subs = new Subscriptions();
  private lastCoreId: string | null = null;

  attach(): void {
    this.subs.attach(() => {
      this.lastCoreId = cores().currentEntry()?.id ?? null;
      return [
        // The only trigger: no poll, no timer, no boot fetch (the
        // no-non-consented-network rule).
        cores().subscribeConnectionState((state) => {
          if (state === "connected") void this.refresh();
        }),
        cores().subscribe(() => {
          const id = cores().currentEntry()?.id ?? null;
          if (id === this.lastCoreId) return;
          this.lastCoreId = id;
          this.current = null;
        }),
      ];
    });
  }

  detach(): void {
    this.subs.detach();
    this.lastCoreId = null;
    this.current = null;
  }

  /** Unknown means capable: a false negative hides a working feature with no
   *  way to discover it, a false positive shows a control that fails with a
   *  worded reason. Also keeps a core that sends no `capabilities` block working. */
  can(flag: keyof CoreCapabilities): boolean {
    return this.current?.[flag] ?? true;
  }

  private async refresh(): Promise<void> {
    const client = cores().currentClient();
    if (!client) return;
    const forCoreId = cores().currentEntry()?.id ?? null;
    try {
      const body = await client.get<HealthResponse>("/api/v1/health");
      // A switch while in flight: don't gate the new core's UI on the old one's answer.
      if ((cores().currentEntry()?.id ?? null) !== forCoreId) return;
      this.current = body.capabilities ?? null;
    } catch (err) {
      // A failed probe is a transport problem, not evidence about the core.
      log.warn("could not read core capabilities", err);
    }
  }
}

export const capabilitiesState = new CapabilitiesState();
