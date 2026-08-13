/**
 * Reactive store for in-flight runtime permission requests. A
 * `tool.permission_request` frame means a running tool is paused on a Deno
 * permission prompt; UserInput switches into its permission mode (textarea
 * and send controls replaced by the request description + X / check
 * buttons) until the user decides. A single prompt blocks its own worker's
 * JS thread, but concurrent tool calls run in separate workers and can each
 * raise one, so requests are queued and surfaced one at a time (the head is
 * `pending`) instead of clobbering a single slot.
 */

import type { PermissionKind } from "@loca/shared";
import { cores } from "$lib/core";
import { messagesState } from "./messages.svelte";

export type PendingPermission = {
  callId: string;
  requestId: string;
  permissionKind: PermissionKind;
  resource: string;
  apiName?: string;
  declared: boolean;
  reason?: string;
  extensionId: string;
  toolName: string;
};

class PermissionState {
  private queue = $state<PendingPermission[]>([]);
  pending = $derived(this.queue[0] ?? null);

  set(req: PendingPermission): void {
    // Dedupe a re-broadcast of the same request; otherwise enqueue.
    if (this.queue.some((p) => p.callId === req.callId && p.requestId === req.requestId)) return;
    this.queue = [...this.queue, req];
  }

  /** Send the user's decision for the head request and surface the next.
   *  The accept is scoped to the current tool call only; "always allow"
   *  lives in the extension detail view. */
  respond(allow: boolean): void {
    const p = this.queue[0];
    if (!p) return;
    this.queue = this.queue.slice(1);
    cores().api().chat.respondPermission(p.callId, p.requestId, allow);
    messagesState.clearToolCallPermissionRequest(p.callId);
  }

  /** Drop any queued requests for a call that reached a terminal state some
   *  other way (timeout, cancel, worker death). */
  clearForCall(callId: string): void {
    this.queue = this.queue.filter((p) => p.callId !== callId);
  }

  /** Drop all pending requests without responding. Used on disconnect: the
   *  owning calls are gone, so a response would only vanish into a dead
   *  socket while UserInput stayed stuck in permission mode. */
  clear(): void {
    this.queue = [];
  }
}

export const permissionState = new PermissionState();
