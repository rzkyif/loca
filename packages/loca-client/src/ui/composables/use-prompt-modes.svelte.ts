/**
 * The schedule-confirm prompt's editable draft: the working copy of a tool's
 * proposed scheduled prompt while the composer is paused on the confirm form,
 * plus its validity gate and the accept/decline response.
 *
 * Per the composable convention, this class holds the `$state` draft and exposes
 * `scheduleDraftReady` as a pure getter; the consumer keeps the `$derived` reads
 * of the pending stores (permissionRequest / scheduleConfirm / inPromptMode) and
 * the `$effect` that mirrors the pending frame's draft into `scheduleDraft`. The
 * permission prompt carries no editable state, so it stays entirely in the
 * consumer's $derived; only the schedule draft needs a home here.
 */

import type { ScheduledPromptDraft } from "@loca/shared";
import { scheduleConfirmState } from "$stores";

export class PromptModes {
  // Editable copy of the pending schedule-confirm draft. The pending frame is
  // reactive deep state, so its draft is a Proxy; this holds a plain editable
  // clone ($state.snapshot, the same call the accept path uses). The consumer's
  // $effect writes it whenever the pending frame changes.
  scheduleDraft = $state<ScheduledPromptDraft | null>(null);

  get scheduleDraftReady(): boolean {
    const d = this.scheduleDraft;
    if (!d) return false;
    if (!d.title.trim() || !d.instruction.trim()) return false;
    // A "once" in the past would never fire; make the user pick a future time.
    if (d.schedule.kind === "once" && d.schedule.atMs <= Date.now()) {
      return false;
    }
    return true;
  }

  respondScheduleConfirm(accepted: boolean): void {
    const draft = this.scheduleDraft;
    scheduleConfirmState.respond(accepted, accepted && draft ? $state.snapshot(draft) : undefined);
  }
}
