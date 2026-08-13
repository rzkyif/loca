// Centralized download manager (port of packages/loca-client/src/tauri/src/download.rs,
// adapted for SQLite persistence + DI-style instantiation).
//
// Concurrency: at most one active download at a time (HF rate-limit friendly).
// Persistence: every state change writes to the `downloads` table (DownloadStore).
// Transfer: the byte streaming, resume, and verification live in transfer.ts.
// Resume: on construction, any persisted Downloading row is reset to Pending
// and re-spawned by `resumePending()`.
// Self-heal: persisted Completed rows whose file no longer exists are dropped.
//
// This file is the orchestrator: it owns the in-flight set, the concurrency
// gate, the awaiter fanout, and the change broadcast, delegating persistence to
// DownloadStore and the transfer to streamTransfer.
//
// Caller surface:
//   - enqueue(spec):  start (or join) a download; resolves to the abs path
//   - cancel(id):     abort an active download or remove a queued one
//   - retry(id):      re-queue a previously-failed download
//   - remove(id):     drop a Completed/Error/Cancelled row from the queue
//   - snapshot():     all rows
//   - markAllSeen():  flip seen=true on every row
//   - subscribe(fn):  fire-on-change observer (used by the WS hub)

import { join } from "@std/path";
import { errMessage } from "@loca/shared";
import type { DownloadEntry } from "@loca/shared";
import { AppError } from "@loca/core-engine";
import { isWithin } from "@loca/core-engine";
import { newJobId } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { Semaphore } from "../shared/semaphore.ts";
import { paths } from "../paths.ts";
import { parseSource } from "./sources.ts";
import { DownloadStore } from "./store.ts";
import { streamTransfer } from "./transfer.ts";

const log = getLogger("downloads");

const INTER_DOWNLOAD_DELAY_MS = 1_000;

export interface EnqueueSpec {
  source: string;
  destination: "models" | "binaries" | "extensions";
  groupId: string;
  sizeHint?: number;
  // Optional SHA-256 (lowercase hex) verified after a successful download.
  // Required for binaries; optional/elided for models (HF supplies its own).
  sha256?: string;
  // Direct-URL mode. When `url` is set, parseSource is bypassed and the
  // caller-supplied url + relPath are used instead. Useful for fetching
  // binaries from the Loca CDN where the source format isn't an HF spec.
  url?: string;
  relPath?: string;
  filename?: string;
}

type Listener = (snapshot: DownloadEntry[]) => void;

interface InFlight {
  abort: AbortController;
  // Awaiters subscribed to this download. Each receives the final result.
  resolvers: Array<{
    resolve: (path: string) => void;
    reject: (err: Error) => void;
  }>;
}

export class DownloadManager {
  private readonly store = new DownloadStore();
  private readonly inFlight = new Map<string, InFlight>();
  private readonly listeners = new Set<Listener>();
  // Single permit: at most one download runs at a time (HF rate-limit friendly).
  private readonly sem = new Semaphore(1);
  // Track scheduled inter-download release timers so shutdown() can cancel
  // any pending ticks and not leave the process holding a stray timer.
  private readonly pendingTimers = new Set<ReturnType<typeof setTimeout>>();

  constructor() {
    this.store.normalizePersistedRows();
  }

  shutdown(): void {
    for (const t of this.pendingTimers) clearTimeout(t);
    this.pendingTimers.clear();
  }

  // Resume any rows that were Pending when the process last exited. Called
  // from main after the manager is constructed and the rest of core is wired.
  resumePending(): void {
    // Skip `binaries` rows: a generic resume only re-runs the byte transfer, but
    // a sidecar binary still needs the extract-into-binDir + version-record step
    // that lives in BinariesManager.kickoff's in-process closure (gone once the
    // process that started it exits), and its sha256 isn't re-verified here. So
    // resuming one would land an unverified archive in staging and never install
    // it. BinariesManager.reconcileInterruptedInstalls() owns finishing those on
    // boot instead (re-resolve + re-download + verify + extract). Models resume
    // normally: they stream straight to their final path with no extraction.
    const rows = this.snapshot().filter(
      (r) => r.status === "Pending" && r.destination !== "binaries",
    );
    for (const row of rows) {
      if (this.inFlight.has(row.id)) continue;
      // Arm the in-flight entry BEFORE spawn, exactly like enqueue/retry do.
      // spawn() no-ops when no in-flight entry exists (its guard), so a resumed
      // row that skipped this would stay Pending forever with no worker: a
      // permanent "queued but never downloads" limbo that ALSO keeps the corebar
      // stuck on "Downloading" (its status counts non-terminal store rows). This
      // is the invariant that removes any divergence: every non-terminal row has
      // a live worker.
      this.inFlight.set(row.id, { abort: new AbortController(), resolvers: [] });
      this.spawn(
        row.id,
        {
          source: row.source,
          destination: row.destination as EnqueueSpec["destination"],
          groupId: row.groupId,
          sizeHint: row.sizeBytes,
        },
        row.absPath,
      ).catch((err) => {
        // spawn should mark the row terminal itself; if it threw before doing so,
        // reconcile here so no worker-less non-terminal row can linger (which
        // would strand the corebar on "Downloading" with nothing progressing).
        // Covers both non-terminal states: a throw before OR after spawn flips
        // the row to Downloading.
        this.inFlight.delete(row.id);
        const status = this.store.getRow(row.id)?.status;
        if (status === "Pending" || status === "Downloading") {
          this.store.setStatus(row.id, "Error", { error: errMessage(err) });
          this.broadcast();
        }
        log.warn(`resume ${row.id}: ${errMessage(err)}`);
      });
    }
  }

  enqueue(spec: EnqueueSpec): Promise<string> {
    const absPath = this.resolveAbsPath(spec);
    const id = downloadId(spec.destination, absPath);

    // Fast path: file already on disk.
    return new Promise<string>((resolve, reject) => {
      this.alreadyOnDisk(absPath)
        .then(async (on) => {
          if (on) {
            await this.store.upsertCompleted(id, spec, absPath);
            this.broadcast();
            resolve(absPath);
            return;
          }

          const existing = this.inFlight.get(id);
          if (existing) {
            existing.resolvers.push({ resolve, reject });
            return;
          }

          const ctrl = new AbortController();
          const inFlight: InFlight = {
            abort: ctrl,
            resolvers: [{ resolve, reject }],
          };
          this.inFlight.set(id, inFlight);
          this.store.upsertPending(id, spec, absPath);
          this.broadcast();
          this.spawn(id, spec, absPath).catch((err) => {
            log.warn(`spawn ${id}: ${errMessage(err)}`);
          });
        })
        // Any synchronous failure in the fast path (DB upsert, broadcast) must
        // reject the returned promise instead of leaving the caller hung.
        .catch(reject);
    });
  }

  cancel(id: string): void {
    const inFlight = this.inFlight.get(id);
    if (inFlight) {
      inFlight.abort.abort();
    } else {
      // Wasn't running; flip the row directly if it's Pending.
      const row = this.store.getRow(id);
      if (row && row.status === "Pending") {
        this.store.setStatus(id, "Cancelled", { error: "cancelled" });
        this.broadcast();
      }
    }
  }

  retry(id: string): void {
    const row = this.store.getRow(id);
    if (!row) return;
    if (row.status === "Downloading" || row.status === "Pending") return;
    if (this.inFlight.has(id)) return;
    this.store.setStatus(id, "Pending", { error: undefined, downloadedBytes: 0 });
    this.broadcast();
    const ctrl = new AbortController();
    this.inFlight.set(id, { abort: ctrl, resolvers: [] });
    this.spawn(
      id,
      {
        source: row.source,
        destination: row.destination as EnqueueSpec["destination"],
        groupId: row.groupId,
        sizeHint: row.sizeBytes,
      },
      row.absPath,
    ).catch((err) => {
      log.warn(`retry ${id}: ${errMessage(err)}`);
    });
  }

  /** The queue-row id `enqueue(spec)` uses for this spec, without enqueuing.
   *  Callers that hand ids back to clients must use this: cancel/retry/remove
   *  match on it, so any other id silently no-ops. */
  idFor(spec: EnqueueSpec): string {
    return downloadId(spec.destination, this.resolveAbsPath(spec));
  }

  remove(id: string): void {
    const row = this.store.getRow(id);
    if (!row) return;
    if (this.inFlight.has(id)) return;
    this.store.delete(id);
    // Drop any resume partial so discarding a download doesn't orphan a .tmp
    // (Error rows keep theirs for resume; removing the row is the cleanup point).
    void Deno.remove(row.absPath + ".tmp").catch(() => {});
    this.broadcast();
  }

  /** Drop Completed rows whose file is no longer on disk (deleted in-app or
   *  externally), so a stale "done" entry self-clears. */
  async reconcileCompleted(): Promise<void> {
    if (await this.store.reconcileCompleted()) this.broadcast();
  }

  snapshot(): DownloadEntry[] {
    return this.store.snapshot();
  }

  markAllSeen(): void {
    // No `seen` column in the schema. The client tracks read state locally.
    // Method preserved on the API for symmetry with the old Tauri command.
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  // --- internals -----------------------------------------------------------

  private resolveAbsPath(spec: EnqueueSpec): string {
    const relPath = spec.relPath ?? parseSource(spec.source).relPath;
    const root = this.destinationRoot(spec.destination);
    const abs = join(root, relPath);
    // `spec.relPath` can be supplied directly (bypassing parseSource's component
    // checks), so guard the final joined path: a `..`-bearing relPath must not
    // write outside the destination root.
    if (!isWithin(root, abs)) {
      throw new AppError("validation_error", `download path escapes ${spec.destination} root`);
    }
    return abs;
  }

  private destinationRoot(dest: EnqueueSpec["destination"]): string {
    const p = paths();
    switch (dest) {
      case "models":
        return p.modelsDir;
      case "binaries":
        return p.binDir;
      case "extensions":
        return p.extensionsDir;
    }
  }

  private async alreadyOnDisk(absPath: string): Promise<boolean> {
    try {
      const st = await Deno.stat(absPath);
      // A zero-byte file is an incomplete/aborted prior download, not a usable
      // artifact: treat it as absent so it re-downloads rather than completing.
      return st.isFile && st.size > 0;
    } catch {
      return false;
    }
  }

  private async spawn(id: string, spec: EnqueueSpec, absPath: string): Promise<void> {
    await this.sem.acquire();
    const inFlight = this.inFlight.get(id);
    if (!inFlight) {
      this.sem.release();
      return;
    }
    if (inFlight.abort.signal.aborted) {
      this.store.setStatus(id, "Cancelled");
      this.fanoutFailure(id, "cancelled");
      // Drop the in-flight entry like every other spawn exit (line ~349). Without
      // this a download cancelled WHILE QUEUED leaks its entry, and the `has(id)`
      // guards then wedge that id forever: retry()/remove() no-op, and a fresh
      // enqueue() joins the dead entry with a resolver that never settles.
      this.inFlight.delete(id);
      this.broadcast();
      this.sem.release();
      return;
    }
    this.store.setStatus(id, "Downloading", { downloadedBytes: 0 });
    this.broadcast();

    let result: { ok: true; path: string } | { ok: false; error: string };
    try {
      const finalBytes = await streamTransfer({
        spec,
        absPath,
        signal: inFlight.abort.signal,
        onProgress: (downloaded) => {
          this.store.updateProgress(id, downloaded);
          this.broadcast();
        },
        onSizeKnown: (total) => this.store.updateSize(id, total),
      });
      this.store.updateProgress(id, finalBytes);
      result = { ok: true, path: absPath };
    } catch (err) {
      const msg = errMessage(err);
      result = { ok: false, error: msg };
    }

    if (result.ok) {
      this.store.setStatus(id, "Completed", { downloadedBytes: undefined });
    } else if (inFlight.abort.signal.aborted) {
      // User-cancelled: drop the partial (the user abandoned this download;
      // remove() also cleans it, but cancel is the explicit "stop" intent).
      this.store.setStatus(id, "Cancelled", { error: result.error });
      try {
        await Deno.remove(absPath + ".tmp");
      } catch {
        /* fine */
      }
    } else {
      // Transient failure (network drop, stall, ...): KEEP the partial .tmp so a
      // retry resumes via a Range request instead of re-downloading from zero
      // (see transfer.ts). Terminal-bad outcomes (checksum mismatch, HTTP 416)
      // already deleted it inside streamTransfer, and remove() drops it if the
      // user discards the download.
      this.store.setStatus(id, "Error", { error: result.error });
    }
    this.broadcast();

    if (result.ok) this.fanoutSuccess(id, result.path);
    else this.fanoutFailure(id, result.error);

    this.inFlight.delete(id);

    // Brief delay between successive downloads so HF doesn't burst-throttle. The
    // permit stays held for the duration (released only when the timer fires),
    // so the throttle is part of the critical section, not a gap after it.
    const handle = setTimeout(() => {
      this.pendingTimers.delete(handle);
      this.sem.release();
    }, INTER_DOWNLOAD_DELAY_MS);
    this.pendingTimers.add(handle);
  }

  // --- fanout --------------------------------------------------------------

  private broadcast(): void {
    if (this.listeners.size === 0) return;
    const snap = this.snapshot();
    for (const l of this.listeners) {
      try {
        l(snap);
      } catch (err) {
        log.warn(`download listener threw: ${errMessage(err)}`);
      }
    }
  }

  private fanoutSuccess(id: string, path: string): void {
    const inFlight = this.inFlight.get(id);
    if (!inFlight) return;
    for (const r of inFlight.resolvers) r.resolve(path);
  }

  private fanoutFailure(id: string, msg: string): void {
    const inFlight = this.inFlight.get(id);
    if (!inFlight) return;
    for (const r of inFlight.resolvers) {
      r.reject(new AppError("provider_error", msg));
    }
  }
}

// --- helpers ---------------------------------------------------------------

function downloadId(dest: EnqueueSpec["destination"], absPath: string): string {
  return `${dest}:${absPath}`;
}

// Singleton accessor for the rest of core to consume.
let _instance: DownloadManager | null = null;
export function downloadManager(): DownloadManager {
  if (!_instance) _instance = new DownloadManager();
  return _instance;
}

export function __resetForTesting(): void {
  _instance = null;
}

// Unused but exported for callers that want to mint an ad-hoc job id.
export { newJobId };
