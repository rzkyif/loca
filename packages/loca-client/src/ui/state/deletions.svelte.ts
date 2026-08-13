/**
 * Reactive store backing the "pending deletions" review modal. The Storage
 * settings field pushes a request describing exactly what will be freed (and
 * what is being skipped because it's in use); the modal renders it and runs the
 * callback on confirm. Modeled on the downloads/confirm stores.
 */

export type DeletionItem = { label: string; sizeBytes: number };
export type SkippedItem = { label: string; reason: string };

export type DeletionRequest = {
  title: string;
  /** Optional lead paragraph (e.g. the factory-reset warning for settings). */
  notice?: string;
  /** Items that will be removed. May be empty when `notice` carries the action
   *  (e.g. settings reset has no file list). */
  items: DeletionItem[];
  /** Items left untouched because they're in use, with the reason. */
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
