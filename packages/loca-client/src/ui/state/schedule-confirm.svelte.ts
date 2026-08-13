/**
 * Reactive store for in-flight schedule confirm requests. A
 * `schedule.confirm_request` frame means a running tool proposed a
 * scheduled prompt; UserInput switches into its schedule-confirm mode (an
 * editable draft form with X / check controls) until the user decides.
 * Concurrent tool calls can each propose one, so requests are queued and
 * surfaced one at a time (the head is `pending`) instead of clobbering a
 * single slot.
 */

import type { ScheduledPromptDraft } from "@loca/shared";
import { cores } from "$lib/core";
import { messagesState } from "./messages.svelte";

export type PendingScheduleConfirm = {
  callId: string;
  requestId: string;
  draft: ScheduledPromptDraft;
};

class ScheduleConfirmState {
  private queue = $state<PendingScheduleConfirm[]>([]);
  pending = $derived(this.queue[0] ?? null);

  set(req: PendingScheduleConfirm): void {
    if (this.queue.some((p) => p.callId === req.callId && p.requestId === req.requestId)) return;
    this.queue = [...this.queue, req];
    messagesState.setToolCallAwaitingSchedule(req.callId);
  }

  /** Send the user's decision for the head request and surface the next.
   *  On accept, `draft` carries the form's (possibly edited) version; core
   *  persists it before unblocking the tool. */
  respond(accepted: boolean, draft?: ScheduledPromptDraft): void {
    const p = this.queue[0];
    if (!p) return;
    this.queue = this.queue.slice(1);
    cores()
      .api()
      .chat.respondScheduleConfirm(
        p.callId,
        p.requestId,
        accepted,
        accepted ? (draft ?? p.draft) : undefined,
      );
    messagesState.clearToolCallAwaitingSchedule(p.callId);
  }

  /** Drop any queued requests for a call that reached a terminal state some
   *  other way (timeout, cancel, worker death). */
  clearForCall(callId: string): void {
    this.queue = this.queue.filter((p) => p.callId !== callId);
  }

  /** Drop all pending requests without responding. Used on disconnect: the
   *  proposing calls are gone, so a response would vanish into a dead socket
   *  while UserInput stayed stuck in schedule-confirm mode. */
  clear(): void {
    this.queue = [];
  }
}

export const scheduleConfirmState = new ScheduleConfirmState();
