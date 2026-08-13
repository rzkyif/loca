/**
 * The askUser prompt's editable answers: the working copy of a tool's pending
 * `ctx.askUser()` form while the composer is paused on it, the per-kind commit
 * actions surfaced as composer buttons, and the response back to the worker.
 *
 * This is the askUser sibling of `PromptModes` (schedule confirm). It holds the
 * `$state` drafts and all the answer/submit logic; the consumer keeps the
 * `$derived` read of the pending askUser frame (off `messagesState`) and mirrors
 * it in via `sync()`. There is no dedicated global store: the request already
 * lives in the tool message's ephemera, so the composer derives it from there.
 */

import type { AskUserAnswer, AskUserChoiceQuestion, AskUserQuestion } from "@loca/shared";
import type { DraftAnswer } from "@loca/shared/ui/components/chat/userinput/AskUserFormView.svelte";
import { extensionsState } from "$stores";

/** The descriptor `PromptButtonsView` renders (icon optional for image actions). */
type PromptButton = {
  icon?: string;
  label: string;
  title: string;
  disabled?: boolean;
  onClick: () => void;
};

export type PendingAsk = {
  callId: string;
  requestId: string;
  questions: AskUserQuestion[];
};

function blankDraft(): DraftAnswer {
  return { text: "", picks: [], freestyleActive: false, rows: [] };
}

// Frames that predate the kind discriminator carry no kind and mean "choice".
function isChoice(q: AskUserQuestion): q is AskUserChoiceQuestion {
  return q.kind === undefined || q.kind === "choice";
}

export class AskUser {
  // The pending askUser frame, mirrored in by the consumer from the tool
  // message's ephemera; null when no tool is awaiting input.
  pending = $state<PendingAsk | null>(null);
  // Live working copy of the answers, indexed by question number.
  drafts = $state<Record<number, DraftAnswer>>({});

  // Tracks which freeform inputs currently have focus. A single-select choice
  // with `allowFreeformInput` defaults to "auto-submit on option click", but
  // focusing the text input flips it into "manual submit" mode.
  private freeformFocused = $state<Record<number, boolean>>({});
  private lastSeenRequestId: string | null = null;
  // Idempotent per requestId: the auto-submit effect and the action buttons
  // both funnel through `submit`, and the status flip after the send is async,
  // so without a guard the same answer would post twice.
  private lastSubmittedRequestId: string | null = null;

  // Mirror the pending frame in and reset the drafts when a new request
  // arrives. The consumer calls this from an $effect tracking the pending frame.
  sync(pending: PendingAsk | null): void {
    this.pending = pending;
    if (!pending) {
      this.lastSeenRequestId = null;
      return;
    }
    if (this.lastSeenRequestId === pending.requestId) return;
    const next: Record<number, DraftAnswer> = {};
    for (let i = 0; i < pending.questions.length; i++) {
      const q = pending.questions[i];
      next[i] = { ...blankDraft(), rows: q.kind === "table" ? q.rows.map((r) => [...r]) : [] };
    }
    this.drafts = next;
    this.freeformFocused = {};
    this.lastSeenRequestId = pending.requestId;
  }

  togglePick = (idx: number, value: string, multi: boolean): void => {
    const d = this.drafts[idx] ?? blankDraft();
    if (multi) {
      const set = new Set(d.picks);
      if (set.has(value)) set.delete(value);
      else set.add(value);
      this.drafts[idx] = { ...d, picks: Array.from(set) };
    } else {
      this.drafts[idx] = { ...d, picks: [value], freestyleActive: false };
    }
  };

  setText = (idx: number, text: string): void => {
    const d = this.drafts[idx] ?? blankDraft();
    const active = text.trim().length > 0;
    const q = this.pending?.questions[idx];
    const multi = !!(q && isChoice(q) && q.multiselect);
    this.drafts[idx] = {
      ...d,
      text,
      picks: !multi && active ? [] : d.picks,
      freestyleActive: active,
    };
  };

  private focusFreestyle(idx: number): void {
    const d = this.drafts[idx] ?? blankDraft();
    if (d.text.trim().length === 0) return;
    const q = this.pending?.questions[idx];
    if (q && isChoice(q) && q.multiselect) return;
    if (d.freestyleActive && d.picks.length === 0) return;
    this.drafts[idx] = { ...d, picks: [], freestyleActive: true };
  }

  onFreestyleFocus = (idx: number): void => {
    this.freeformFocused[idx] = true;
    this.focusFreestyle(idx);
  };

  onFreestyleBlur = (idx: number): void => {
    this.freeformFocused[idx] = false;
  };

  // Table-grid editing. Rows are replaced immutably so the $state proxy sees
  // every change; an empty grid is a valid answer ("none of these").
  setCell = (idx: number, row: number, col: number, value: string): void => {
    const d = this.drafts[idx] ?? blankDraft();
    if (!d.rows[row]) return;
    const rows = d.rows.map((r) => [...r]);
    rows[row][col] = value;
    this.drafts[idx] = { ...d, rows };
  };

  addRow = (idx: number, columns: number): void => {
    const d = this.drafts[idx] ?? blankDraft();
    this.drafts[idx] = { ...d, rows: [...d.rows, Array.from({ length: columns }, () => "")] };
  };

  removeRow = (idx: number, row: number): void => {
    const d = this.drafts[idx] ?? blankDraft();
    this.drafts[idx] = { ...d, rows: d.rows.filter((_, i) => i !== row) };
  };

  get readyToSubmit(): boolean {
    const p = this.pending;
    if (!p) return false;
    for (let i = 0; i < p.questions.length; i++) {
      const q = p.questions[i];
      const d = this.drafts[i] ?? blankDraft();
      if (!isChoice(q)) {
        if (q.kind === "table") continue;
        if (q.kind === "files" && q.multiselect) continue;
        if (d.picks.length === 0) return false;
        continue;
      }
      if (!q.options) {
        if (!d.text.trim()) return false;
        continue;
      }
      const hasFreestyle = !!q.allowFreeformInput && d.freestyleActive && !!d.text.trim();
      if (d.picks.length === 0 && !hasFreestyle) return false;
    }
    return true;
  }

  // Whether the form needs an explicit Submit. Plain-text, multiselect (choice
  // or files), and table questions always count; diff, image, and single-select
  // files resolve from one click. For single-select-with-freeform, the question
  // only counts while the freeform input is engaged (focused, or has typed text).
  get requiresSubmit(): boolean {
    const p = this.pending;
    if (!p) return false;
    for (let i = 0; i < p.questions.length; i++) {
      const q = p.questions[i];
      if (q.kind === "diff" || q.kind === "image") continue;
      if (q.kind === "files") {
        if (q.multiselect) return true;
        continue;
      }
      if (q.kind === "table") return true;
      if (!q.options) return true;
      if (q.multiselect) return true;
      if (q.allowFreeformInput) {
        const engaged =
          this.freeformFocused[i] === true || (this.drafts[i]?.freestyleActive ?? false);
        if (engaged) return true;
      }
    }
    return false;
  }

  private answerFor(q: AskUserQuestion, d: DraftAnswer): AskUserAnswer {
    if (q.kind === "diff" || q.kind === "image") return d.picks[0] ?? "";
    if (q.kind === "files") return q.multiselect ? d.picks : (d.picks[0] ?? "");
    if (q.kind === "table") {
      return d.rows.map((row) => Object.fromEntries(q.columns.map((c, i) => [c, row[i] ?? ""])));
    }
    if (!q.options) return d.text.trim();
    const text = d.text.trim();
    if (q.multiselect) {
      const all = [...d.picks];
      if (q.allowFreeformInput && text.length > 0) all.push(text);
      return all;
    }
    const useFreestyle = !!q.allowFreeformInput && d.freestyleActive && text.length > 0;
    return useFreestyle ? text : (d.picks[0] ?? "");
  }

  submit = (): void => {
    const p = this.pending;
    if (!p) return;
    if (this.lastSubmittedRequestId === p.requestId) return;
    const answers: AskUserAnswer[] = [];
    for (let i = 0; i < p.questions.length; i++) {
      answers.push(this.answerFor(p.questions[i], this.drafts[i] ?? blankDraft()));
    }
    this.lastSubmittedRequestId = p.requestId;
    extensionsState.respondAskUser(p.callId, p.requestId, answers);
  };

  // The commit actions surfaced as composer buttons, styled like the permission
  // Deny / Allow pair. A lone diff/image question hoists its verdict/action
  // buttons here (mirrored by AskUserFormView's `hoistActions` gate); otherwise
  // a single Submit appears when an explicit commit is required, and single-
  // select choice/files commit by clicking the option tile in the content.
  get actions(): PromptButton[] {
    const p = this.pending;
    if (!p) return [];
    const qs = p.questions;
    if (qs.length === 1) {
      const q = qs[0];
      if (q.kind === "diff") {
        return [
          {
            icon: "i-material-symbols-close-rounded",
            label: "Reject",
            title: "Reject this change",
            onClick: () => {
              this.togglePick(0, "reject", false);
              this.submit();
            },
          },
          {
            icon: "i-material-symbols-check-rounded",
            label: "Accept",
            title: "Accept this change",
            onClick: () => {
              this.togglePick(0, "accept", false);
              this.submit();
            },
          },
        ];
      }
      if (q.kind === "image") {
        return q.actions.map((a) => ({
          label: a.label,
          title: a.label,
          onClick: () => {
            this.togglePick(0, a.value, false);
            this.submit();
          },
        }));
      }
    }
    if (this.requiresSubmit) {
      return [
        {
          icon: "i-material-symbols-check-rounded",
          label: "Submit",
          title: "Submit your answer",
          disabled: !this.readyToSubmit,
          onClick: () => this.submit(),
        },
      ];
    }
    return [];
  }
}
