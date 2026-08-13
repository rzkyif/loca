/**
 * Reactive per-sidecar status facade, rebuilt from the aggregate `core.status`
 * WS frame's `subsystems` breakdown (the per-sidecar `sidecar.status` frame was
 * folded into `core.status`). Components read `serversState.serverStatuses` to
 * render running / loading / error / disabled state per sidecar (the Services
 * metrics table, the chat input placeholder + voice gating).
 */

import type { ServerToClientFrame, SidecarKind, SidecarStatus } from "@loca/shared";
import { cores } from "$lib/core";
import { Subscriptions } from "$lib/util/subscriptions";

export type ServerStatusUpdate = {
  server: SidecarKind;
  status: SidecarStatus;
  message?: string;
};

// Every sidecar kind seeded to Disabled, so a render that reads a status (e.g.
// the chat input's voice gating) never hits an undefined entry, and a kind the
// core never started (commonly `tool`) reads as Disabled. Rebuilt fresh on each
// frame, so a subsystem that drops off the breakdown reverts to Disabled.
function freshStatuses(): Record<SidecarKind, ServerStatusUpdate> {
  return {
    llama: { server: "llama", status: "Disabled" },
    "llama-embed": { server: "llama-embed", status: "Disabled" },
    speech: { server: "speech", status: "Disabled" },
    tool: { server: "tool", status: "Disabled" },
  };
}

class ServersState {
  serverStatuses = $state<Record<SidecarKind, ServerStatusUpdate>>(freshStatuses());

  private subs = new Subscriptions();

  /** Wire to the WS hub of the currently-selected core. Idempotent. */
  attach(): void {
    this.subs.attach(() => [
      cores().subscribeWs((frame: ServerToClientFrame) => {
        if (frame.kind !== "core.status") return;
        const next = freshStatuses();
        for (const s of frame.snapshot.subsystems ?? []) {
          next[s.kind] = { server: s.kind, status: s.status, message: s.message };
        }
        this.serverStatuses = next;
      }),
    ]);
  }

  detach(): void {
    this.subs.detach();
  }
}

export const serversState = new ServersState();
