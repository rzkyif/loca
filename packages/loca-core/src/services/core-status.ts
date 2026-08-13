// Aggregate core lifecycle status (Starting Up / Downloading / Idle / Busy /
// Updating / Error). A single read-only aggregator that the WS hub subscribes
// to and rebroadcasts as a `core.status` frame, and that the health route reads
// to seed a freshly-connecting client.
//
// Inputs arrive two ways:
//   - PULLED on each recompute: sidecar readiness (sidecarManager.getStatuses).
//   - PUSHED by the owning subsystem: chat active-turn count (services/chat.ts),
//     LLM queue (services/llm-scheduler.ts), speech queue
//     (services/speech-scheduler.ts), boot completion + download activity
//     (main.ts), and update staging (subscribeUpdate, wired in the constructor
//     path here).
//
// Push (not pull) for the dynamic signals keeps this module free of imports
// back into chat / the schedulers, so there is no import cycle: those modules
// import `coreStatus()` and call a `note*` setter; this module never imports
// them. Emission is edge-only (a deep-equal guard) so transient sub-state churn
// does not spam frames, mirroring the hub's `lastDownloadStatus` pattern.

import type {
  CoreQueues,
  CoreStatusSnapshot,
  DownloadEntry,
  SidecarSnapshot,
  SubsystemStatus,
} from "@loca/shared";
import { sidecarManager } from "../sidecars/manager.ts";
import { subscribeUpdate } from "../update/self-updater.ts";
import { getLogger } from "../shared/log.ts";

const log = getLogger("core-status");

type Listener = (snapshot: CoreStatusSnapshot) => void;

function sameSubsystems(a: SubsystemStatus[], b: SubsystemStatus[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i].kind !== b[i].kind || a[i].status !== b[i].status || a[i].message !== b[i].message) {
      return false;
    }
  }
  return true;
}

function sameQueues(a: CoreQueues | undefined, b: CoreQueues | undefined): boolean {
  if (!a && !b) return true;
  if (!a || !b) return false;
  return (
    a.llmActive === b.llmActive &&
    a.llmQueued === b.llmQueued &&
    a.speechActive === b.speechActive &&
    a.speechQueued === b.speechQueued &&
    a.activeStreams === b.activeStreams
  );
}

function sameSnapshot(a: CoreStatusSnapshot, b: CoreStatusSnapshot): boolean {
  return (
    a.status === b.status &&
    a.detail === b.detail &&
    a.progress === b.progress &&
    sameSubsystems(a.subsystems, b.subsystems) &&
    sameQueues(a.queues, b.queues)
  );
}

export class CoreStatusService {
  // True from process start until main() reports the HTTP listener is bound and
  // sidecar boot has been kicked. While set, the core cannot serve yet.
  private booting = true;
  // Set when a self-update is staged; the core is about to restart.
  private updating = false;
  // In-flight chat turns (generating, or awaiting / executing a tool).
  private activeStreams = 0;
  // Local LLM queue: any in-flight or queued completion.
  private llmQueued = 0;
  private llmActive = 0;
  // Local speech (STT/TTS) queue.
  private speechQueued = 0;
  private speechActive = 0;
  // Active (Pending/Downloading) downloads, pushed by main.ts's downloadManager
  // subscription. While files are still being fetched the sidecars that need
  // them sit at Disabled, so without this the bar would read Idle mid-fetch.
  private downloadsActive = 0;
  private downloadsProgress: number | undefined = undefined;

  private listeners = new Set<Listener>();
  private last: CoreStatusSnapshot = { status: "starting_up", subsystems: [] };
  private wired = false;

  // The sidecar status source. Defaults to the live manager; injectable so the
  // subsystem / error derivation is unit-testable without spawning processes.
  constructor(
    private readonly sidecarSource: () => SidecarSnapshot[] = () => sidecarManager().getStatuses(),
  ) {}

  /** Subscribe to sidecar + update events once the singleton is first used.
   *  Idempotent; called from `coreStatus()`. */
  wire(): void {
    if (this.wired) return;
    this.wired = true;
    sidecarManager().subscribe(() => this.recompute());
    subscribeUpdate((e) => {
      if (e.kind === "staged") {
        this.updating = true;
        this.recompute();
      }
    });
    this.recompute();
  }

  snapshot(): CoreStatusSnapshot {
    return this.last;
  }

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  // --- pushed signals ------------------------------------------------------

  noteBootDone(): void {
    if (!this.booting) return;
    this.booting = false;
    this.recompute();
  }

  noteActiveStreams(n: number): void {
    if (this.activeStreams === n) return;
    this.activeStreams = n;
    this.recompute();
  }

  noteLlmQueue(active: number, queued: number): void {
    if (this.llmActive === active && this.llmQueued === queued) return;
    this.llmActive = active;
    this.llmQueued = queued;
    this.recompute();
  }

  noteSpeechQueue(active: number, queued: number): void {
    if (this.speechActive === active && this.speechQueued === queued) return;
    this.speechActive = active;
    this.speechQueued = queued;
    this.recompute();
  }

  /** Fold a download-manager snapshot into the aggregate. Progress is rounded
   *  to whole percent so per-chunk byte updates don't spam status frames. */
  noteDownloads(entries: DownloadEntry[]): void {
    const active = entries.filter((e) => e.status === "Pending" || e.status === "Downloading");
    // Aggregate progress over the size-known active entries only: an unsized
    // entry would add downloaded bytes with no total and push the ratio past 1.
    const sized = active.filter((e) => typeof e.sizeBytes === "number" && e.sizeBytes > 0);
    const total = sized.reduce((n, e) => n + (e.sizeBytes ?? 0), 0);
    const done = sized.reduce((n, e) => n + Math.min(e.downloadedBytes, e.sizeBytes ?? 0), 0);
    const progress = total > 0 ? Math.round((done / total) * 100) / 100 : undefined;
    if (this.downloadsActive === active.length && this.downloadsProgress === progress) return;
    this.downloadsActive = active.length;
    this.downloadsProgress = progress;
    this.recompute();
  }

  // --- derivation ----------------------------------------------------------

  recompute(): void {
    const next = this.derive();
    if (sameSnapshot(next, this.last)) return;
    this.last = next;
    for (const fn of this.listeners) {
      try {
        fn(next);
      } catch (err) {
        log.warn(`core-status listener threw: ${String(err)}`);
      }
    }
  }

  private derive(): CoreStatusSnapshot {
    const sidecars = this.sidecarSource();
    // Per-subsystem breakdown rides every snapshot: it folds the old per-sidecar
    // frame (the client rebuilds `serversState` from it) and feeds the CoreBar's
    // expanded error card. The Map's insertion order is stable, so the list is
    // deep-compared positionally by `sameSubsystems`.
    const subsystems: SubsystemStatus[] = sidecars.map((s) => ({
      kind: s.kind,
      status: s.status,
      message: s.message,
    }));

    // 1. Error: a sidecar the manager actually started has hit a terminal Error
    //    (gated-off sidecars sit at Disabled and never reach here). The
    //    collapsed `detail` names the first one; the expanded card lists them
    //    all from `subsystems`.
    const errored = sidecars.find((s) => s.status === "Error");
    if (errored) {
      return {
        status: "error",
        detail: errored.message ?? `${errored.kind} failed`,
        subsystems,
      };
    }

    // 2. Updating: a build is staged; a restart is imminent.
    if (this.updating) return { status: "updating", subsystems };

    // 3. Starting up: still booting, or a sidecar is loading its model.
    const loading = sidecars.find((s) => s.status === "Loading");
    if (this.booting || loading) {
      return {
        status: "starting_up",
        detail: loading ? `loading ${loading.kind}` : undefined,
        progress: loading?.progress,
        subsystems,
      };
    }

    // 4. Busy: a turn is generating, or speech/LLM work is in flight or queued.
    //    Carry the queue counts so the expanded card can show what is working
    //    and waiting, per queue.
    const queued = this.llmQueued + this.speechQueued;
    if (this.activeStreams > 0 || this.llmActive > 0 || this.speechActive > 0 || queued > 0) {
      return {
        status: "busy",
        detail: queued > 0 ? `${queued} queued` : undefined,
        subsystems,
        queues: {
          llmActive: this.llmActive,
          llmQueued: this.llmQueued,
          speechActive: this.speechActive,
          speechQueued: this.speechQueued,
          activeStreams: this.activeStreams,
        },
      };
    }

    // 5. Downloading: required files (models / sidecar binaries) are still
    //    being fetched. Below busy so live turn activity (e.g. an external
    //    provider serving chat during a local-model download) still shows.
    if (this.downloadsActive > 0) {
      return {
        status: "downloading",
        detail: this.downloadsActive === 1 ? "1 file" : `${this.downloadsActive} files`,
        progress: this.downloadsProgress,
        subsystems,
      };
    }

    // 6. Idle.
    return { status: "idle", subsystems };
  }
}

let _instance: CoreStatusService | null = null;
export function coreStatus(): CoreStatusService {
  if (!_instance) {
    _instance = new CoreStatusService();
    _instance.wire();
  }
  return _instance;
}

// Test-only: drops the cached aggregator so the next `coreStatus()` rebuilds it.
export function __resetForTesting(): void {
  _instance = null;
}
