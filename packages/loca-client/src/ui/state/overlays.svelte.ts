// Stores backing the global modal / overlay surfaces (confirm, admin password,
// bubble color picker, pending deletions) and the queued tool prompts a running
// tool can raise (runtime permission, schedule confirm). Anywhere in the app
// pushes a request here and the matching component renders it.

import type { DownloadPlan, PermissionKind, ScheduledPromptDraft } from "@loca/shared";
import { cores } from "$lib/core";
import { messagesState } from "./messages.svelte";

type ConfirmRequest = {
  title: string;
  message: string;
  confirmLabel?: string;
  /** Defaults to "Cancel". */
  cancelLabel?: string;
  destructive?: boolean;
  /** `dontShowAgain` carries the checkbox value when `dontShowAgainLabel` rendered one. */
  onConfirm: (dontShowAgain?: boolean) => void | Promise<void>;
  onCancel?: () => void;
  /** One-button notice (no Cancel). */
  alert?: boolean;
  /** Render the download list with a size total alongside the message. */
  downloads?: DownloadPlan[];
  dontShowAgainLabel?: string;
};

class ConfirmState {
  pending = $state<ConfirmRequest | null>(null);

  request(req: ConfirmRequest) {
    this.pending = req;
  }

  alert(opts: { title: string; message: string; confirmLabel?: string }) {
    this.pending = {
      title: opts.title,
      message: opts.message,
      confirmLabel: opts.confirmLabel ?? "OK",
      alert: true,
      onConfirm: () => {},
    };
  }

  cancel() {
    const p = this.pending;
    this.pending = null;
    p?.onCancel?.();
  }

  async confirm(dontShowAgain?: boolean) {
    const p = this.pending;
    this.pending = null;
    if (p) await p.onConfirm(dontShowAgain);
  }
}

export const confirmState = new ConfirmState();

type PasswordPromptRequest = {
  title: string;
  message?: string;
  /** Defaults to "Continue". */
  confirmLabel?: string;
  /** Throw to keep the modal open and surface the error; resolve to close it. */
  onSubmit: (password: string) => Promise<void>;
  onCancel?: () => void;
};

class PasswordPromptState {
  pending = $state<PasswordPromptRequest | null>(null);

  request(req: PasswordPromptRequest) {
    this.pending = req;
  }

  cancel() {
    const p = this.pending;
    this.pending = null;
    p?.onCancel?.();
  }
}

export const passwordPromptState = new PasswordPromptState();

type ColorPickerRequest = {
  anchor: HTMLElement;
  /** An `oklch(...)` string, or legacy hex. */
  initialColor: string;
  onApply: (hex: string) => void;
  /** Hide the lightness slider and pin L to 0.5: seed colors' lightness is
   *  ignored by the theme ladders, so editing it would mislead. */
  lockLightness?: boolean;
};

class ColorPickerState {
  pending = $state<ColorPickerRequest | null>(null);

  open(req: ColorPickerRequest) {
    this.pending = req;
  }

  close() {
    this.pending = null;
  }

  apply(hex: string) {
    const p = this.pending;
    this.pending = null;
    p?.onApply(hex);
  }
}

export const colorPickerState = new ColorPickerState();

export type DeletionItem = { label: string; sizeBytes: number };
export type SkippedItem = { label: string; reason: string };

export type DeletionRequest = {
  title: string;
  notice?: string;
  /** May be empty when `notice` carries the action (settings reset). */
  items: DeletionItem[];
  /** Left untouched because in use. */
  skipped: SkippedItem[];
  confirmLabel?: string;
  onConfirm: () => void | Promise<void>;
};

class DeletionsState {
  pending = $state<DeletionRequest | null>(null);

  request(req: DeletionRequest) {
    this.pending = req;
  }

  cancel() {
    this.pending = null;
  }

  async confirm() {
    const p = this.pending;
    this.pending = null;
    if (p) await p.onConfirm();
  }
}

export const deletionsState = new DeletionsState();

// Concurrent tool calls can each raise a prompt, so requests queue and surface
// one at a time (the head is `pending`) instead of clobbering a single slot.
class ToolPromptQueue<T extends { callId: string; requestId: string }> {
  protected queue = $state<T[]>([]);
  pending = $derived(this.queue[0] ?? null);

  /** Enqueue unless it is a re-broadcast of a queued request. */
  set(req: T): boolean {
    if (this.queue.some((p) => p.callId === req.callId && p.requestId === req.requestId)) {
      return false;
    }
    this.queue = [...this.queue, req];
    return true;
  }

  protected shift(): T | null {
    const p = this.queue[0];
    if (p) this.queue = this.queue.slice(1);
    return p ?? null;
  }

  /** Drop queued requests for a call that ended some other way (timeout, cancel, worker death). */
  clearForCall(callId: string): void {
    this.queue = this.queue.filter((p) => p.callId !== callId);
  }

  /** Drop everything without responding: on disconnect the owning calls are
   *  gone and a response would vanish into a dead socket. */
  clear(): void {
    this.queue = [];
  }
}

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

class PermissionState extends ToolPromptQueue<PendingPermission> {
  /** Scoped to the current tool call; "always allow" lives in the extension detail view. */
  respond(allow: boolean): void {
    const p = this.shift();
    if (!p) return;
    cores().api().chat.respondPermission(p.callId, p.requestId, allow);
    messagesState.clearToolCallPermissionRequest(p.callId);
  }
}

export const permissionState = new PermissionState();

export type PendingScheduleConfirm = {
  callId: string;
  requestId: string;
  draft: ScheduledPromptDraft;
};

class ScheduleConfirmState extends ToolPromptQueue<PendingScheduleConfirm> {
  override set(req: PendingScheduleConfirm): boolean {
    const added = super.set(req);
    if (added) messagesState.setToolCallAwaitingSchedule(req.callId);
    return added;
  }

  /** On accept, `draft` carries the form's (possibly edited) version. */
  respond(accepted: boolean, draft?: ScheduledPromptDraft): void {
    const p = this.shift();
    if (!p) return;
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
}

export const scheduleConfirmState = new ScheduleConfirmState();
