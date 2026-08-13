/**
 * Reactive mirror of the core's download queue. Fed by `downloads.snapshot`
 * WS broadcasts; mutations go through the core's REST endpoints.
 *
 * `seen` state is purely client-local (the core doesn't track read state),
 * so a refresh resets the seen-set.
 */

import type { DownloadEntry, RequiredFile, ServerToClientFrame } from "@loca/shared";
import { errMessage } from "@loca/shared";
import { cores } from "$lib/core";
import { platform } from "$lib/platform";
import { getLogger } from "$lib/util/log";
import { Subscriptions } from "$lib/util/subscriptions";
import { confirmState } from "./confirm.svelte";

const log = getLogger("downloads");
const reqLog = getLogger("requirements");

/** True when a core's files live on this same device, i.e. its base URL is
 *  loopback. Only then can we reveal a downloaded file in the local file
 *  manager (a remote core's paths don't exist on this machine). */
function isLocalBaseUrl(baseUrl: string | undefined): boolean {
  if (!baseUrl) return false;
  try {
    const host = new URL(baseUrl).hostname.toLowerCase();
    return host === "127.0.0.1" || host === "localhost" || host === "::1";
  } catch {
    return false;
  }
}

class DownloadsState {
  items = $state<DownloadEntry[]>([]);
  modalOpen = $state(false);
  flashingIds = $state<Set<string>>(new Set());
  /** Per-id "seen" tracking lives only in the client process. */
  seen = $state<Set<string>>(new Set());

  // Authoritative required-files snapshot from the core (one source of truth).
  // `missing` is what the pending-downloads popup renders and the app gates on.
  required = $state<RequiredFile[]>([]);
  missing = $state<RequiredFile[]>([]);
  // False until the first authoritative requirements snapshot lands (WS frame
  // or HTTP fetch) for the current core. The UI gates on `loading` so boot and
  // core-switch show a loading state instead of defaulting to "ready" and then
  // flipping to "pending" once a snapshot reveals missing files.
  requirementsLoaded = $state(false);
  // True when the current core is on this same device (loopback). Gates the
  // "reveal in file manager" affordance, since a remote core's file paths don't
  // exist on this machine. Refreshed whenever snapshots are (re)seeded.
  localCore = $state(false);
  // Missing-file sources the user has approved downloading (via the Pending
  // Downloads modal). Pruned on every snapshot to whatever is still missing, so
  // approval persists as the batch downloads + shrinks, but a newly-surfaced
  // requirement (e.g. switching model) needs approval again. Drives the
  // download button: pending->approve flips it to download-manager mode while
  // the app stays gated until the files actually land.
  approvedSources = $state<Set<string>>(new Set());

  active = $derived(this.items.filter((i) => i.status === "Pending" || i.status === "Downloading"));
  completed = $derived(this.items.filter((i) => i.status === "Completed"));
  errored = $derived(this.items.filter((i) => i.status === "Error"));
  cancelled = $derived(this.items.filter((i) => i.status === "Cancelled"));
  hasAny = $derived(this.items.length > 0);

  hasPending = $derived(this.missing.length > 0);
  /** Missing files the user hasn't approved downloading yet. While true the
   *  download button stays in "Pending Downloads" mode (opens the confirm
   *  modal); once approved it flips to download-manager mode even though the app
   *  is still gated (`hasPending`) until the files finish downloading. */
  needsApproval = $derived(this.missing.some((m) => !this.approvedSources.has(m.source)));
  /** Missing files whose last prepare/download attempt failed (core couldn't
   *  resolve/install them). Retryable: the button and popup offer another go. */
  failed = $derived(this.missing.filter((m) => m.error));
  /** Approved and genuinely in progress: pending, nothing awaiting approval, and
   *  nothing failed. Every surface derives "download activity" from the
   *  authoritative `missing` set this way, NOT from the transfer queue, so the
   *  chat gate and the settings button can never disagree. */
  installing = $derived(this.hasPending && !this.needsApproval && this.failed.length === 0);
  /** True until the first requirements snapshot arrives for the current core:
   *  callers show a loading state rather than assuming "ready". */
  loading = $derived(!this.requirementsLoaded);
  /** Order-independent signature of the missing set, for the "Do It Later"
   *  dismissal logic: the popup re-shows when this changes. */
  missingSignature = $derived(
    this.missing
      .map((m) => m.source)
      .sort()
      .join("|"),
  );

  /** Whether the last queue snapshot had any in-flight transfer, for detecting
   *  the active -> idle settling edge that triggers a requirements self-heal. */
  private queueActive = false;

  private subs = new Subscriptions();

  attach(): void {
    this.subs.attach(() => [
      cores().subscribeWs((frame: ServerToClientFrame) => {
        if (frame.kind === "downloads.snapshot") {
          const wasActive = this.queueActive;
          this.items = frame.items;
          this.queueActive = frame.items.some(
            (i) => i.status === "Pending" || i.status === "Downloading",
          );
          // Self-heal: the queue just settled (was active, now idle) while the
          // app is still gated. Re-pull the authoritative requirements snapshot
          // so a missed/late requirements delta - or a binary install that
          // failed to enqueue anything at all - can't leave `missing` stale and
          // the UI stuck. Edge-triggered, so a persistently-failed binary
          // refetches once and then stops (no idle queue -> no further edges).
          if (wasActive && !this.queueActive && this.hasPending) {
            void this.refetchRequirements();
          }
        } else if (frame.kind === "requirements.snapshot") {
          this.applyRequirements(frame.required, frame.missing);
        }
      }),
      // The core pushes downloads/requirements as deltas over the WS but never
      // replays a full snapshot on connect, so (re)seed over HTTP whenever the
      // socket reaches "connected". onConnectionState fires synchronously with
      // the current state on subscribe, so this also covers the very first
      // connect. Covering reconnect is what un-sticks the UI after the core
      // restarts (dev hot-reload): without it, requirementsLoaded could stay
      // false forever.
      cores().subscribeConnectionState((state) => {
        if (state === "connected") this.reseedSnapshots();
      }),
    ]);
  }

  /** Pull the REST-only snapshots (downloads + requirements) for the current
   *  core. Skipped when no core is paired yet (cores().api() throws). */
  private reseedSnapshots(): void {
    this.localCore = isLocalBaseUrl(cores().currentEntry()?.baseUrl);
    if (!cores().currentEntry()) return;
    void cores()
      .api()
      .models.downloads()
      .then((items) => {
        this.items = items;
      })
      .catch((e) => {
        log.warn("snapshot fetch failed:", e);
      });
    void this.refetchRequirements();
  }

  /** Pull the requirements snapshot over HTTP. Load-bearing at startup (the
   *  WS snapshot may be broadcast before this client's socket connects) and on
   *  core switch. */
  async refetchRequirements(): Promise<void> {
    if (!cores().currentEntry()) return;
    // Re-enter the loading state until the snapshot lands so a core switch
    // can't briefly show the previous core's (or a default "ready") state.
    this.requirementsLoaded = false;
    try {
      const snap = await cores().api().requirements.get();
      this.applyRequirements(snap.required, snap.missing);
    } catch (e) {
      reqLog.warn("initial fetch failed:", e);
    }
  }

  /** Apply an authoritative requirements snapshot. Prunes `approvedSources` to
   *  what is still missing, so approval persists across the batch shrinking but
   *  is forgotten once a file lands (so re-deleting it prompts again). */
  private applyRequirements(required: RequiredFile[], missing: RequiredFile[]): void {
    this.required = required;
    this.missing = missing;
    const stillMissing = new Set(missing.map((m) => m.source));
    this.approvedSources = new Set([...this.approvedSources].filter((s) => stillMissing.has(s)));
    this.requirementsLoaded = true;
  }

  /** Build + fire the "Pending Downloads" confirm modal from the current
   *  `missing` set. `hooks` lets the Settings auto-popup layer its dismissal
   *  bookkeeping on top; the sidebar button calls it with no hooks to force a
   *  re-show while pending.
   *
   *  `probe` gates the outbound size HEADs to HuggingFace/GitHub. Only an
   *  explicit user action may set it (the sidebar button click) - that is the
   *  consent surface for the network request, per the no-non-consented-network
   *  rule. The Settings auto-popup fires reactively on connect, NOT on a user
   *  action, so it must leave `probe` false and render the local (unsized)
   *  snapshot; the confirm modal shows "+" for unknown sizes. */
  async requestRequiredModal(opts?: {
    probe?: boolean;
    onConfirm?: () => void;
    onCancel?: () => void;
  }): Promise<void> {
    const hooks = opts;
    // Size probe only on the consented (explicit-click) path. On failure, fall
    // back to the current (unsized) `missing` set so the modal still renders.
    if (opts?.probe) {
      try {
        const snap = await cores().api().requirements.get({ probe: true });
        this.applyRequirements(snap.required, snap.missing);
      } catch (e) {
        reqLog.warn("size probe for pending-downloads modal failed:", e);
      }
    }
    const plans = this.missing.map((m) => ({
      source: m.source,
      alreadyHave: false,
      sizeHint: m.sizeHint,
      version: m.version,
      error: m.error,
    }));
    confirmState.request({
      title: "Pending Downloads",
      message:
        `The following file${plans.length === 1 ? "" : "s"} need${
          plans.length === 1 ? "s" : ""
        } to be downloaded ` + `so the Core can run with the current configuration.`,
      confirmLabel: "Download",
      cancelLabel: "Do It Later",
      downloads: plans,
      onConfirm: async () => {
        hooks?.onConfirm?.();
        // Mark everything currently missing as approved up front, so the button
        // flips to download-manager mode immediately (the app stays gated on
        // `hasPending` until the files actually land).
        const prevApproved = this.approvedSources;
        this.approvedSources = new Set([
          ...this.approvedSources,
          ...this.missing.map((m) => m.source),
        ]);
        try {
          await cores().api().requirements.download();
          // Re-pull requirements: if a required binary couldn't be resolved,
          // nothing was enqueued and no queue-settle edge will fire, so this is
          // what surfaces the failure (retryable) instead of a silent limbo.
          void this.refetchRequirements();
        } catch (e) {
          // The request failed, so nothing was enqueued. Roll back the
          // optimistic approval: otherwise `needsApproval` reads false and the
          // download button drops out of "Pending Downloads" while the chat
          // stays gated on `hasPending`, leaving a split, stuck UI. Surface the
          // failure too, so it isn't silently swallowed and the user knows to
          // retry.
          this.approvedSources = prevApproved;
          reqLog.warn("download failed:", e);
          confirmState.alert({ title: "Download failed", message: errMessage(e) });
        }
      },
      onCancel: () => hooks?.onCancel?.(),
    });
  }

  detach(): void {
    this.subs.detach();
  }

  openModal(): void {
    const unseen = this.items
      .filter((i) => i.status === "Completed" && !this.seen.has(i.id))
      .map((i) => i.id);
    this.flashingIds = new Set(unseen);
    this.modalOpen = true;
    for (const id of this.items.map((i) => i.id)) this.seen.add(id);
    setTimeout(() => {
      this.flashingIds = new Set();
    }, 2000);
  }

  closeModal(): void {
    this.modalOpen = false;
  }

  async cancel(id: string): Promise<void> {
    try {
      await cores().api().models.cancel(id);
    } catch (e) {
      log.warn("cancel failed:", e);
    }
  }

  async retry(id: string): Promise<void> {
    try {
      await cores().api().models.retry(id);
    } catch (e) {
      log.warn("retry failed:", e);
    }
  }

  async remove(id: string): Promise<void> {
    try {
      await cores().api().models.remove(id);
    } catch (e) {
      log.warn("remove failed:", e);
    }
  }

  async clearCompleted(): Promise<void> {
    const ids = this.completed.map((c) => c.id);
    for (const id of ids) {
      try {
        await cores().api().models.remove(id);
      } catch (e) {
        log.warn(`clearCompleted: remove(${id}) failed:`, e);
      }
    }
  }

  async reveal(absPath: string): Promise<void> {
    // Only meaningful for a same-device core; the UI only shows the affordance
    // when `localCore` is true, so the path exists on this machine.
    if (!this.localCore) return;
    try {
      await platform().revealPath(absPath);
    } catch (e) {
      log.warn("reveal failed:", e);
    }
  }
}

export const downloadsState = new DownloadsState();
