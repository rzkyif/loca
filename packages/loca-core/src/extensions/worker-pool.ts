// Extension worker pool. Per-(extension, tool) warm subprocess with LRU eviction +
// idle timeout. Spawn flags are computed from ONLY the invoked tool's granted
// permissions (least privilege): a benign tool never runs in a process that
// holds a sibling tool's net / run / ffi grants.
//
// Behaviorally rich part of the extension subsystem; ports the semantics of
// src/bun/extensions/worker/pool.ts to Deno subprocesses.

import { newCallId } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { numSetting } from "@loca/core-engine/services/settings-access";
import { paths } from "../paths.ts";
import { AppError } from "@loca/core-engine";
import { emptyFlagSet, flagSetToArgs, type PathTemplates, unionFlags } from "./permissions.ts";
import { extensionsRegistry } from "./registry.ts";
import { WorkerHandle } from "./worker-handle.ts";
import { defaultDownloadsDir, resolveEntryPath } from "./worker-entry-resolution.ts";
import {
  type CallController,
  type CallEvent,
  InFlightCall,
  type PoolCallbacks,
  type ToolCallStart,
} from "./worker-call.ts";

const log = getLogger("workerpool");

export interface PoolConfig {
  maxWarmWorkers: number;
  workerIdleMs: number;
  callTimeoutMs: number;
  drainTimeoutMs: number;
}

export const DEFAULT_POOL_CONFIG: PoolConfig = {
  maxWarmWorkers: 8,
  workerIdleMs: 300_000,
  callTimeoutMs: 60_000,
  drainTimeoutMs: 2_000,
};

/** The settings that tune the pool. Named as a set, and paired with a test
 *  asserting every key `poolConfigFrom` reads is in it: D-139 is a watch set
 *  that omitted two of the keys its own reader used, so the user changed a
 *  setting and nothing happened. `drainTimeoutMs` is absent because it is not a
 *  setting. */
export const POOL_KEYS = new Set([
  "extensions.maxWarmWorkers",
  "extensions.workerIdleMs",
  "extensions.callTimeoutMs",
]);

/** Read the pool's tunables from a settings record.
 *
 *  Every fallback is `DEFAULT_POOL_CONFIG`'s own value, which is also the schema
 *  default, so a sparse record lands where the panel says it should. Zero and
 *  negative are refused rather than applied: zero warm workers means no tool can
 *  ever run, and a zero timeout fails every call instantly. */
export function poolConfigFrom(s: Record<string, unknown>): PoolConfig {
  const positive = (key: string, fallback: number): number => {
    const value = numSetting(s, key, fallback);
    return value > 0 ? value : fallback;
  };
  return {
    maxWarmWorkers: Math.max(
      1,
      Math.floor(positive("extensions.maxWarmWorkers", DEFAULT_POOL_CONFIG.maxWarmWorkers)),
    ),
    workerIdleMs: positive("extensions.workerIdleMs", DEFAULT_POOL_CONFIG.workerIdleMs),
    callTimeoutMs: positive("extensions.callTimeoutMs", DEFAULT_POOL_CONFIG.callTimeoutMs),
    drainTimeoutMs: DEFAULT_POOL_CONFIG.drainTimeoutMs,
  };
}

export class WorkerPool {
  private config: PoolConfig = DEFAULT_POOL_CONFIG;
  private workers = new Map<string, WorkerHandle>();
  private lru: string[] = []; // extensionIds in MRU order
  private idleTimers = new Map<string, ReturnType<typeof setTimeout>>();

  setConfig(cfg: Partial<PoolConfig>): void {
    this.config = { ...this.config, ...cfg };
  }

  // Execute a single tool call. Returns a controller for cancel/askUser
  // forwarding; the `done` promise settles with the tool's return value
  // (resolved on tool_result) or rejects on tool_error / cancel / timeout.
  // The call's lifecycle and prompt/timeout state lives in InFlightCall; the
  // pool keeps the warm-worker accounting it drives through PoolCallbacks (so
  // `worker.inFlightCalls` is decremented from exactly one place per settle).
  startCall(spec: ToolCallStart, onEvent: (event: CallEvent) => void): CallController {
    const callId = newCallId();
    // Workers are keyed per (extension, tool), not per extension, so each tool runs
    // with ONLY its own granted permissions (least privilege) instead of the
    // union of every enabled tool's grants. `key` is this call's worker
    // identity for the pool/LRU/idle-timer maps.
    const key = workerKey(spec.extensionId, spec.tool.name);
    const worker = this.getOrSpawn(spec, key);

    const callbacks: PoolCallbacks = {
      onStarted: () => {
        worker.inFlightCalls++;
        this.clearIdleTimer(key);
      },
      onSettled: () => {
        worker.inFlightCalls--;
        this.retireOrIdle(key, worker);
      },
      onKilled: () => {
        worker.inFlightCalls = Math.max(0, worker.inFlightCalls - 1);
        this.killWorker(key, worker);
      },
      onBootFailed: () => {
        worker.inFlightCalls = Math.max(0, worker.inFlightCalls - 1);
        this.bumpIdleTimer(key);
      },
    };

    return new InFlightCall({
      callId,
      worker,
      spec,
      onEvent,
      callbacks,
      callTimeoutMs: this.config.callTimeoutMs,
      drainTimeoutMs: this.config.drainTimeoutMs,
    });
  }

  // Kill every warm worker for `extensionId` (e.g. after a grant change). Workers
  // are keyed per (extension, tool), so a single extension can have several; tear
  // them all down. Any in-flight calls receive tool_error via the worker's
  // exit -> rejection path.
  async refreshPermissions(extensionId: string): Promise<void> {
    const prefix = workerKey(extensionId, "");
    const keys = [...this.workers.keys()].filter((k) => k.startsWith(prefix));
    if (keys.length === 0) return;
    log.info(`refreshPermissions: terminating ${keys.length} warm worker(s) for ${extensionId}`);
    for (const key of keys) {
      const w = this.workers.get(key);
      if (!w) continue;
      await w.terminate(this.config.drainTimeoutMs);
      this.workers.delete(key);
      this.removeFromLru(key);
      this.clearIdleTimer(key);
    }
  }

  async shutdown(): Promise<void> {
    await Promise.all(
      Array.from(this.workers.values(), (w) => w.terminate(this.config.drainTimeoutMs)),
    );
    this.workers.clear();
    this.lru = [];
    for (const t of this.idleTimers.values()) clearTimeout(t);
    this.idleTimers.clear();
  }

  stats(): {
    warmWorkers: number;
    maxWarmWorkers: number;
    inFlightCalls: number;
  } {
    let total = 0;
    for (const w of this.workers.values()) total += w.inFlightCalls;
    return {
      warmWorkers: this.workers.size,
      maxWarmWorkers: this.config.maxWarmWorkers,
      inFlightCalls: total,
    };
  }

  // --- internals ---------------------------------------------------------

  private getOrSpawn(spec: ToolCallStart, key: string): WorkerHandle {
    const existing = this.workers.get(key);
    if (existing) {
      this.touchLru(key);
      return existing;
    }
    if (this.workers.size >= this.config.maxWarmWorkers) {
      if (!this.evictLeastRecent()) {
        throw new AppError(
          "server_busy",
          `all ${this.config.maxWarmWorkers} warm extension workers are busy; retry after a tool call drains`,
        );
      }
    }
    return this.spawn(spec, key);
  }

  private spawn(spec: ToolCallStart, key: string): WorkerHandle {
    const extension = extensionsRegistry().getOrThrow(spec.extensionId);
    // Least privilege: the worker's --allow-* flags come from ONLY the tool
    // being invoked, not the union of every enabled tool in the extension. So a
    // benign tool can't run in a process that holds a sibling tool's net / run
    // / ffi grants. The invoked tool's persisted required-permissions + grants
    // are looked up from the registry by name.
    const tool = extensionsRegistry()
      .listTools(spec.extensionId)
      .find((t) => t.name === spec.tool.name);
    const grantContexts = tool ? [{ required: tool.requiredPermissions, grants: tool.grants }] : [];
    const templates: PathTemplates = {
      home: Deno.env.get("HOME") ?? Deno.env.get("USERPROFILE") ?? "",
      downloads: defaultDownloadsDir(),
      models: paths().modelsDir,
      sessions: paths().sessionsDir,
      extension: extension.installedPath,
    };
    const flagSet =
      grantContexts.length > 0 ? unionFlags(grantContexts, templates) : emptyFlagSet();

    // Surface elevated grants in the log so an operator notices an over-broad
    // permission (these escape the Deno sandbox or expose much of $home; the
    // secret vault itself is always denied, see worker-handle.ts). Informational
    // only; the grant was already approved by the user.
    const elevated: string[] = [];
    if (flagSet.ffi) elevated.push("ffi");
    if (flagSet.run.size > 0) {
      elevated.push(`run(${[...flagSet.run].join(",")})`);
    }
    if (templates.home) {
      for (const p of [...flagSet.read, ...flagSet.write]) {
        if (p === templates.home || p === templates.home + "/") {
          elevated.push(`home-wide(${p})`);
        }
      }
    }
    if (elevated.length > 0) {
      log.warn(
        `tool ${spec.extensionId}/${spec.tool.name} granted elevated permissions: ${elevated.join(
          ", ",
        )}`,
      );
    }

    const flags = flagSetToArgs(flagSet);

    const w = WorkerHandle.spawn({
      extensionId: spec.extensionId,
      entryPath: resolveEntryPath(extension.installedPath),
      extensionFolder: extension.installedPath,
      flags,
      // Runtime prompt policy: ask-state (or undeclared, per extension policy)
      // accesses pause on Deno's prompt and route through prompt-matcher.ts.
      promptContext: tool
        ? {
            required: tool.requiredPermissions,
            grants: tool.grants,
            undeclaredPolicy: extension.undeclaredPolicy,
            templates,
          }
        : undefined,
    });
    this.workers.set(key, w);
    this.touchLru(key);
    return w;
  }

  // Normal call-settle path: return the worker to the warm set, UNLESS a
  // user-answered permission prompt happened during its lifetime. Deno caches
  // the per-resource verdict for the process lifetime and prompt answers are
  // scoped to a single call, so such a worker must not serve another call.
  private retireOrIdle(key: string, worker: WorkerHandle): void {
    if (worker.promptAnsweredByUser) {
      log.info(`retiring worker after user-answered permission prompt (${key.split("\u0000")[0]})`);
      this.killWorker(key, worker);
      return;
    }
    this.bumpIdleTimer(key);
  }

  // Force-kill a specific worker and drop it from the pool. Used when a call
  // times out or a cancel goes unacked: the worker is wedged/CPU-bound, so we
  // can't reuse it. The identity check avoids tearing down a replacement worker
  // that may already have taken this key.
  private killWorker(key: string, worker: WorkerHandle): void {
    if (this.workers.get(key) === worker) {
      this.workers.delete(key);
      this.removeFromLru(key);
      this.clearIdleTimer(key);
    }
    void worker.terminate(this.config.drainTimeoutMs);
  }

  private evictLeastRecent(): boolean {
    // Walk LRU head-to-tail (oldest first) and evict the first idle worker.
    // In-flight workers are skipped without disturbing LRU order; if every
    // worker is busy, returns false so the caller can refuse the spawn
    // instead of silently exceeding maxWarmWorkers.
    for (const id of this.lru) {
      const w = this.workers.get(id);
      if (!w || w.inFlightCalls > 0) continue;
      log.info(`evicting idle worker for ${id}`);
      void w.terminate(this.config.drainTimeoutMs);
      this.workers.delete(id);
      this.removeFromLru(id);
      this.clearIdleTimer(id);
      return true;
    }
    return false;
  }

  private touchLru(extensionId: string): void {
    this.removeFromLru(extensionId);
    this.lru.push(extensionId);
  }
  private removeFromLru(extensionId: string): void {
    const i = this.lru.indexOf(extensionId);
    if (i !== -1) this.lru.splice(i, 1);
  }

  private bumpIdleTimer(extensionId: string): void {
    this.clearIdleTimer(extensionId);
    const w = this.workers.get(extensionId);
    if (!w) return;
    if (w.inFlightCalls > 0) return;
    const t = setTimeout(() => {
      log.info(`idle eviction for ${extensionId}`);
      void w.terminate(this.config.drainTimeoutMs);
      this.workers.delete(extensionId);
      this.removeFromLru(extensionId);
      this.idleTimers.delete(extensionId);
    }, this.config.workerIdleMs);
    this.idleTimers.set(extensionId, t);
  }
  private clearIdleTimer(extensionId: string): void {
    const t = this.idleTimers.get(extensionId);
    if (t !== undefined) {
      clearTimeout(t);
      this.idleTimers.delete(extensionId);
    }
  }
}

let _instance: WorkerPool | null = null;
export function workerPool(): WorkerPool {
  if (!_instance) _instance = new WorkerPool();
  return _instance;
}

export function __resetForTesting(): void {
  _instance = null;
}

/** Pool/LRU/idle-timer identity for a worker: one process per (extension, tool)
 *  so each tool runs with only its own permissions. The NUL separator can't
 *  appear in a extension id or tool name, so `startsWith(workerKey(id, ""))`
 *  safely matches every tool-worker of a extension (used by refreshPermissions). */
function workerKey(extensionId: string, toolName: string): string {
  return `${extensionId}\u0000${toolName}`;
}
