/**
 * Reactive mirror of self-update events broadcast by the core over WS.
 *
 * Fed by `update.staged` and `update.error` frames. Holds the most recent
 * staged-version notice and the most recent error (each with a timestamp);
 * UI components subscribe to either field to render banners/toasts/etc.
 *
 * Two fields rather than a queue because both events are inherently
 * "latest wins": once staged, you restart; if a new error fires before
 * the old one was dismissed, the new one is what matters.
 */

import type { ErrorCode, ServerToClientFrame } from "@loca/shared";
import { cores } from "$lib/core";
import { Subscriptions } from "$lib/util/subscriptions";

class UpdateState {
  staged = $state<{ version: string; atMs: number } | null>(null);
  lastError = $state<{ code: ErrorCode; message: string; atMs: number } | null>(null);

  private subs = new Subscriptions();

  attach(): void {
    this.subs.attach(() => [
      cores().subscribeWs((frame: ServerToClientFrame) => {
        if (frame.kind === "update.staged") {
          this.staged = { version: frame.version, atMs: Date.now() };
        } else if (frame.kind === "update.error") {
          this.lastError = {
            code: frame.code,
            message: frame.message,
            atMs: Date.now(),
          };
        }
      }),
    ]);
  }

  detach(): void {
    this.subs.detach();
  }

  dismissError(): void {
    this.lastError = null;
  }

  dismissStaged(): void {
    this.staged = null;
  }
}

export const updateState = new UpdateState();
