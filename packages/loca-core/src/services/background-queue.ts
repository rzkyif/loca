// Single-active FIFO queue for deferrable LLM/embedding work (memory
// summaries, embedding refreshes). Jobs are deduped by key and drain one at
// a time, only while the chat service is idle: no in-flight turn, plus a
// short quiet period after the last one, so background work never competes
// with a user-visible stream for the model. Nothing is persisted; every job
// is re-derivable from source hashes on boot, so dropped jobs are re-found.

import { errMessage } from "@loca/shared";
import { chatService } from "@loca/core-engine/services/chat";
import { getLogger } from "../shared/log.ts";

const log = getLogger("bgqueue");

const QUIET_PERIOD_MS = 5_000;
const BUSY_POLL_MS = 5_000;
// Re-enqueue delay for a job that found a prerequisite missing (e.g. the
// embedding model is not downloaded yet). Long enough not to busy-spin while
// the state persists, short enough to pick the work up once it clears.
const DEFERRED_RETRY_MS = 60_000;

export interface BackgroundJob {
  // Dedupe identity, e.g. "doc-summary:<id>". A key already waiting in the
  // queue is not enqueued again. The key frees when its job STARTS, so a
  // re-enqueue during the run (source changed mid-job) queues a fresh pass.
  key: string;
  run: () => Promise<void>;
}

export interface BackgroundQueueOptions {
  // Test seams; production uses the chat service and the defaults.
  isBusy?: () => boolean;
  quietPeriodMs?: number;
  busyPollMs?: number;
}

export class BackgroundQueue {
  private readonly isBusy: () => boolean;
  private readonly quietPeriodMs: number;
  private readonly busyPollMs: number;
  private queue: BackgroundJob[] = [];
  private keys = new Set<string>();
  private running = false;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private deferredTimers = new Set<ReturnType<typeof setTimeout>>();
  private disposed = false;
  // Boot counts as busy so the first job waits out one quiet period.
  private lastBusyAt = Date.now();

  constructor(options: BackgroundQueueOptions = {}) {
    this.isBusy = options.isBusy ?? (() => chatService().activeSessionIds().size > 0);
    this.quietPeriodMs = options.quietPeriodMs ?? QUIET_PERIOD_MS;
    this.busyPollMs = options.busyPollMs ?? BUSY_POLL_MS;
  }

  enqueue(job: BackgroundJob): void {
    if (this.disposed || this.keys.has(job.key)) return;
    this.keys.add(job.key);
    this.queue.push(job);
    this.schedule(0);
  }

  /** Enqueue after a delay. For a job that ran but found a prerequisite
   *  missing and wants to retry later without busy-spinning the drain loop. */
  enqueueDeferred(job: BackgroundJob): void {
    if (this.disposed) return;
    const t = setTimeout(() => {
      this.deferredTimers.delete(t);
      this.enqueue(job);
    }, DEFERRED_RETRY_MS);
    this.deferredTimers.add(t);
  }

  /** Jobs waiting (not counting one currently running). */
  size(): number {
    return this.queue.length;
  }

  dispose(): void {
    this.disposed = true;
    if (this.timer !== undefined) clearTimeout(this.timer);
    this.timer = undefined;
    for (const t of this.deferredTimers) clearTimeout(t);
    this.deferredTimers.clear();
    this.queue = [];
    this.keys.clear();
  }

  private schedule(delayMs: number): void {
    if (this.disposed || this.timer !== undefined || this.running) return;
    this.timer = setTimeout(() => {
      this.timer = undefined;
      void this.drainOne();
    }, delayMs);
  }

  private async drainOne(): Promise<void> {
    if (this.disposed || this.running || this.queue.length === 0) return;
    if (this.isBusy()) {
      this.lastBusyAt = Date.now();
      this.schedule(this.busyPollMs);
      return;
    }
    const quietFor = Date.now() - this.lastBusyAt;
    if (quietFor < this.quietPeriodMs) {
      this.schedule(this.quietPeriodMs - quietFor);
      return;
    }
    const job = this.queue.shift()!;
    this.keys.delete(job.key);
    this.running = true;
    try {
      await job.run();
    } catch (err) {
      log.warn(`background job ${job.key} failed: ${errMessage(err)}`);
    } finally {
      this.running = false;
      if (this.queue.length > 0) this.schedule(0);
    }
  }
}

let _instance: BackgroundQueue | null = null;
export function backgroundQueue(): BackgroundQueue {
  if (!_instance) _instance = new BackgroundQueue();
  return _instance;
}

// Test-only: disposes and drops the cached queue.
export function __resetForTesting(): void {
  _instance?.dispose();
  _instance = null;
}
