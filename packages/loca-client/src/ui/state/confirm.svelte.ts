/**
 * Reactive store backing the global confirm dialog. Anywhere in the app
 * can push a request onto this state and the dialog component will
 * render it and run the callback when the user confirms, so prompts
 * don't have to be wired through the component tree.
 */

import type { DownloadPlan } from "@loca/shared";

type ConfirmRequest = {
  title: string;
  message: string;
  confirmLabel?: string;
  /** Label for the dismiss button. Defaults to "Cancel". */
  cancelLabel?: string;
  destructive?: boolean;
  /** `dontShowAgain` is set only when `dontShowAgainLabel` rendered a
   *  checkbox; it carries the checkbox value at confirm time. */
  onConfirm: (dontShowAgain?: boolean) => void | Promise<void>;
  onCancel?: () => void;
  /** When true, render as a one-button notice (no Cancel). The `onConfirm`
   *  callback is optional in this mode. */
  alert?: boolean;
  /** When set, render the list of files to download with a size total
   *  alongside the message. */
  downloads?: DownloadPlan[];
  /** When set, render a checkbox with this label above the button row. */
  dontShowAgainLabel?: string;
};

class ConfirmState {
  pending = $state<ConfirmRequest | null>(null);

  request(req: ConfirmRequest) {
    this.pending = req;
  }

  /** Shortcut for a single-button error notice. */
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
