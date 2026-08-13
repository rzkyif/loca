// Speech (STT + TTS) concurrency scheduler. The local speech sidecar is a
// single process whose HTTP server serializes one inference at a time, so
// concurrent multi-client requests would otherwise pile up at the socket with
// no fairness or backpressure. This mirrors LlmScheduler: at most
// `parallelSlots` concurrent local calls, queued work dispatched per-client
// round-robin so one client can't monopolize the engine, and a queue deeper
// than `parallelSlots * 4` returns `server_busy` immediately.
//
// External STT (OpenAI-compatible) bypasses this entirely, exactly like the LLM
// scheduler: it enforces its own limits and local queueing would only add
// latency. Only the single-sidecar local path is scheduled here.
//
// Unlike the LLM scheduler this wraps a single request/response promise, not a
// streamed async iterator, so the surface is `schedule(clientId, fn)`.

import { AppError } from "@loca/core-engine";
import { coreStatus } from "./core-status.ts";

// The speech sidecar runs one engine; STT and TTS share its threads, so a
// single global slot is the safe default (raising it would let an STT and a TTS
// call thrash the same process).
const DEFAULT_PARALLEL_SLOTS = 1;
const QUEUE_DEPTH_MULTIPLIER = 4;

export class SpeechScheduler {
  private localActive = 0;
  private localQueueByClient = new Map<string, Array<() => void>>();
  private clientOrder: string[] = [];
  private nextClientIdx = 0;
  private parallelSlots = DEFAULT_PARALLEL_SLOTS;

  private maxQueueDepth(): number {
    return this.parallelSlots * QUEUE_DEPTH_MULTIPLIER;
  }

  private queueLen(): number {
    let n = 0;
    for (const q of this.localQueueByClient.values()) n += q.length;
    return n;
  }

  private notifyStatus(): void {
    coreStatus().noteSpeechQueue(this.localActive, this.queueLen());
  }

  /** Run `fn` under the local speech semaphore. Rejects with `server_busy`
   *  before acquiring when the queue is already saturated. */
  async schedule<T>(clientId: string, fn: () => Promise<T>): Promise<T> {
    if (this.queueLen() >= this.maxQueueDepth()) {
      throw new AppError(
        "server_busy",
        `speech queue full (${this.queueLen()} waiting, ${this.parallelSlots} slots)`,
      );
    }
    await this.acquire(clientId);
    try {
      return await fn();
    } finally {
      this.release();
    }
  }

  // --- local semaphore + round-robin ---------------------------------------

  private acquire(clientId: string): Promise<void> {
    if (this.localActive < this.parallelSlots) {
      this.localActive++;
      this.notifyStatus();
      return Promise.resolve();
    }
    return new Promise<void>((resolve) => {
      let q = this.localQueueByClient.get(clientId);
      if (!q) {
        q = [];
        this.localQueueByClient.set(clientId, q);
        this.clientOrder.push(clientId);
      }
      q.push(() => {
        this.localActive++;
        this.notifyStatus();
        resolve();
      });
      this.notifyStatus();
    });
  }

  private release(): void {
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

let _instance: SpeechScheduler | null = null;
export function speechScheduler(): SpeechScheduler {
  if (!_instance) _instance = new SpeechScheduler();
  return _instance;
}

// Test-only: drops the cached scheduler so the next call rebuilds a fresh queue.
export function __resetForTesting(): void {
  _instance = null;
}
