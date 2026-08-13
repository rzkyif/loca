// LLM concurrency scheduler (plan §3).
//
// For local llama-server: at most parallelSlots concurrent in-flight
// completions; queued work picks per-client round-robin so a single client
// can't monopolize the model. Queue depth > parallelSlots * 4 returns
// server_busy immediately.
//
// External providers (OpenAI etc.) bypass the semaphore entirely. They
// enforce their own rate limits, and queueing locally would just add
// latency without protecting anything.

import { AppError } from "../platform/errors.ts";
import { errMessage } from "@loca/shared";
import { getLogger } from "../platform/log.ts";
import { host } from "../platform/runtime.ts";
import { type LlmDelta, type LlmRequest, streamChatCompletion } from "./llm-provider.ts";

const log = getLogger("llm-scheduler");

// Serial by default: one in-flight local completion at a time, the rest queue.
// Must match `--parallel` on the llama-server command line (sidecars/llama.ts),
// which runs a single slot so each turn gets the whole context window.
const DEFAULT_PARALLEL_SLOTS = 1;
const QUEUE_DEPTH_MULTIPLIER = 4;

// Per the plan §3 watchdog: "if a slot exceeds callTimeoutMs + 60 s, abort
// upstream, mark llama-server unhealthy, restart". We can't replay queued
// requests at this layer (the stream is owned by services/chat.ts), but
// we abort the wedged upstream and trigger a restart so subsequent
// requests get a healthy server.
const DEFAULT_CALL_TIMEOUT_MS = 60_000;
const WATCHDOG_GRACE_MS = 60_000;

export interface ScheduleOptions {
  clientId: string;
  // Whether the upstream is the local llama-server (subject to semaphore)
  // or an external provider (bypassed).
  isLocal: boolean;
  // How many concurrent local completions the llama-server is configured for.
  // Equal to `--parallel N` on the llama-server command line.
  parallelSlots?: number;
}

export type WatchdogHandler = (info: { clientId: string; elapsedMs: number }) => void;

export class LlmScheduler {
  private localActive = 0;
  private localQueueByClient = new Map<string, Array<() => void>>();
  private clientOrder: string[] = [];
  private nextClientIdx = 0;
  private parallelSlots = DEFAULT_PARALLEL_SLOTS;
  private callTimeoutMs = DEFAULT_CALL_TIMEOUT_MS;
  private watchdogHandler: WatchdogHandler | null = null;

  setParallelSlots(n: number): void {
    if (!Number.isInteger(n) || n < 1) {
      throw new AppError("validation_error", `invalid parallelSlots: ${n}`);
    }
    this.parallelSlots = n;
  }

  setCallTimeoutMs(ms: number): void {
    if (!Number.isFinite(ms) || ms < 1_000) {
      throw new AppError("validation_error", `invalid callTimeoutMs: ${ms}`);
    }
    this.callTimeoutMs = ms;
  }

  // Wire a callback the scheduler invokes when a local slot stays held
  // past `callTimeoutMs + WATCHDOG_GRACE_MS`. Typical handler: restart the
  // llama-server sidecar. The scheduler itself only aborts the upstream
  // stream and releases the slot; restart logic lives in the supervisor.
  setWatchdogHandler(fn: WatchdogHandler | null): void {
    this.watchdogHandler = fn;
  }

  private maxQueueDepth(): number {
    return this.parallelSlots * QUEUE_DEPTH_MULTIPLIER;
  }

  private queueLen(): number {
    let n = 0;
    for (const q of this.localQueueByClient.values()) n += q.length;
    return n;
  }

  /** Push the current local-queue metrics to the core-status aggregator so the
   *  Busy state reflects in-flight + queued LLM work. */
  private notifyStatus(): void {
    host().status?.noteLlmQueue(this.localActive, this.queueLen());
  }

  // Streams a chat completion through the scheduler. Yields the same
  // LlmDelta sequence as streamChatCompletion, with semaphore acquisition
  // gating local provider work.
  async *schedule(req: LlmRequest, opts: ScheduleOptions): AsyncIterable<LlmDelta> {
    if (opts.isLocal) {
      if (opts.parallelSlots !== undefined) {
        this.setParallelSlots(opts.parallelSlots);
      }
      if (this.queueLen() >= this.maxQueueDepth()) {
        throw new AppError(
          "server_busy",
          `llama-server queue full (${this.queueLen()} waiting, ` + `${this.parallelSlots} slots)`,
        );
      }
      await this.acquireLocal(opts.clientId, req.signal);
      // Combine the caller's abort signal (if any) with our watchdog
      // signal so an expired watchdog aborts the upstream cleanly.
      const watchdogController = new AbortController();
      const combinedSignal = req.signal
        ? AbortSignal.any([req.signal, watchdogController.signal])
        : watchdogController.signal;
      const wrappedReq: LlmRequest = { ...req, signal: combinedSignal };
      const startedAt = Date.now();
      const watchdogTimeoutMs = this.callTimeoutMs + WATCHDOG_GRACE_MS;
      // Stall watchdog: fires only after watchdogTimeoutMs of NO progress (no
      // delta), not on total duration. A legitimately long-but-progressing local
      // completion (a big thinking model on CPU) is never aborted; only a truly
      // wedged server (no first token, or no further tokens) trips it. We re-arm
      // the timer on every yielded delta.
      let watchdogTimer: ReturnType<typeof setTimeout> | undefined;
      const armWatchdog = (): void => {
        if (watchdogTimer !== undefined) clearTimeout(watchdogTimer);
        watchdogTimer = setTimeout(() => {
          log.error(
            `watchdog: local slot made no progress for ${watchdogTimeoutMs}ms ` +
              `(client ${opts.clientId}); aborting upstream`,
          );
          try {
            watchdogController.abort(
              new AppError(
                "internal_error",
                `llm scheduler watchdog stall after ${watchdogTimeoutMs}ms`,
              ),
            );
          } catch {
            /* signal already aborted */
          }
          try {
            this.watchdogHandler?.({
              clientId: opts.clientId,
              elapsedMs: Date.now() - startedAt,
            });
          } catch (err) {
            log.warn(`watchdog handler threw: ${errMessage(err)}`);
          }
        }, watchdogTimeoutMs);
      };
      try {
        armWatchdog(); // covers time-to-first-token (a server that never emits)
        for await (const delta of streamChatCompletion(wrappedReq)) {
          armWatchdog(); // progress: reset the stall timer
          yield delta;
        }
      } finally {
        if (watchdogTimer !== undefined) clearTimeout(watchdogTimer);
        this.releaseLocal();
      }
      return;
    }
    // External provider path: no local concurrency control.
    yield* streamChatCompletion(req);
  }

  // --- local semaphore + round-robin ---------------------------------------

  private acquireLocal(clientId: string, signal?: AbortSignal): Promise<void> {
    if (signal?.aborted) return Promise.reject(signal.reason);
    if (this.localActive < this.parallelSlots) {
      this.localActive++;
      this.notifyStatus();
      return Promise.resolve();
    }
    return new Promise<void>((resolve, reject) => {
      let q = this.localQueueByClient.get(clientId);
      if (!q) {
        q = [];
        this.localQueueByClient.set(clientId, q);
        this.clientOrder.push(clientId);
      }
      let onAbort: (() => void) | undefined;
      const grant = (): void => {
        if (signal && onAbort) signal.removeEventListener("abort", onAbort);
        this.localActive++;
        this.notifyStatus();
        resolve();
      };
      q.push(grant);
      // If the turn is interrupted WHILE QUEUED, drop its grant so it never
      // consumes a slot for an already-aborted turn (which would then start an
      // upstream request that immediately aborts, having held up the queue).
      if (signal) {
        onAbort = () => {
          this.removeQueuedGrant(clientId, grant);
          this.notifyStatus();
          reject(signal.reason);
        };
        signal.addEventListener("abort", onAbort, { once: true });
      }
      this.notifyStatus();
    });
  }

  // Remove a still-queued grant (an interrupted turn), pruning the client's queue
  // and clientOrder entry when it empties, mirroring dispatch()'s bookkeeping.
  private removeQueuedGrant(clientId: string, grant: () => void): void {
    const q = this.localQueueByClient.get(clientId);
    if (!q) return;
    const idx = q.indexOf(grant);
    if (idx !== -1) q.splice(idx, 1);
    if (q.length === 0) {
      this.localQueueByClient.delete(clientId);
      const ci = this.clientOrder.indexOf(clientId);
      if (ci !== -1) {
        this.clientOrder.splice(ci, 1);
        if (this.nextClientIdx > ci) this.nextClientIdx--;
      }
    }
  }

  private releaseLocal(): void {
    this.localActive--;
    this.dispatch();
    this.notifyStatus();
  }

  private dispatch(): void {
    if (this.clientOrder.length === 0) return;
    if (this.localActive >= this.parallelSlots) return;
    const startIdx = this.nextClientIdx % this.clientOrder.length;
    for (let i = 0; i < this.clientOrder.length; i++) {
      const idx = (startIdx + i) % this.clientOrder.length;
      const clientId = this.clientOrder[idx];
      const q = this.localQueueByClient.get(clientId);
      if (q && q.length > 0) {
        const next = q.shift();
        if (q.length === 0) {
          this.localQueueByClient.delete(clientId);
          this.clientOrder.splice(idx, 1);
          this.nextClientIdx = idx; // keep rotation point stable after splice
        } else {
          this.nextClientIdx = idx + 1;
        }
        next?.();
        return;
      }
    }
  }
}

let _instance: LlmScheduler | null = null;
export function llmScheduler(): LlmScheduler {
  if (!_instance) _instance = new LlmScheduler();
  return _instance;
}

// Test-only: drops the cached scheduler so the next `llmScheduler()` call
// rebuilds it with a fresh queue.
export function __resetForTesting(): void {
  _instance = null;
}

// Suppress unused-import warning in build output.
void log;
