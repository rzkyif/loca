/**
 * Reactive store backing the global admin-password prompt. A privileged core
 * action (generate a pairing key, remove another device) pushes a request
 * here; the modal collects the password and runs `onSubmit`. If `onSubmit`
 * throws (e.g. the core rejects a wrong password) the modal stays open and
 * shows the error so the user can retry; a clean return closes it.
 */

type PasswordPromptRequest = {
  title: string;
  /** Optional context line under the title. */
  message?: string;
  /** Label for the submit button. Defaults to "Continue". */
  confirmLabel?: string;
  /** Run with the entered password. Throw to keep the modal open and surface
   *  the error; resolve to close it. */
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
