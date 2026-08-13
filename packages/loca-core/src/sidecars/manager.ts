// Sidecar supervisor (port of packages/loca-client/src/tauri/src/sidecar.rs).
//
// Responsibilities:
//   - Spawn / re-spawn sidecar subprocesses.
//   - Supersede in-flight starts via a monotonic startId.
//   - Health-check (HTTP poll or stdout marker or warmup sleep) and transition
//     from Loading -> Running -> Error.
//   - Capture last 10 output lines for error messaging on unexpected exit.
//   - Graceful kill: SIGTERM, GRACEFUL_SHUTDOWN_MS grace, SIGKILL on Unix;
//     SIGKILL on Windows.
//   - Restart-on-crash with exponential backoff up to maxAttempts.
//   - Broadcast SidecarSnapshot to subscribers (the ws hub forwards over WS).
//
// Out of scope here (handled by sibling modules per the rework plan):
//   - Per-kind arg construction (sidecars/{llama,llama-embed,speech}.ts).
//   - Windows Job Object sidecar tracking (sidecars/jobctl.ts).
//   - Binary path resolution (binaries/manager.ts).

import { getLogger } from "../shared/log.ts";
import { errMessage } from "@loca/shared";
import { trackSidecarPid } from "./jobctl.ts";
import { libraryEnvFor } from "./library-path.ts";
import { pollHttpHealth, sleep, STARTUP_WARMUP_MS, validateHealthCheckUrl } from "./readiness.ts";
import {
  DEFAULT_RESTART_POLICY,
  type RestartPolicy,
  type SidecarKind,
  type SidecarSnapshot,
  type StartOptions,
  type StatusListener,
} from "./types.ts";

const log = getLogger("sidecars");

// Environment variables that the sidecar binaries (llama-server,
// loca-core-speech) legitimately need. We deliberately do NOT inherit the
// core's full environment, which can carry operator secrets (e.g. GITHUB_TOKEN
// used by the upstream resolver) that these binaries have no business seeing.
// The dynamic-library path is added separately via libraryEnvFor().
const SIDECAR_ENV_ALLOWLIST = [
  "PATH",
  "HOME",
  "TMPDIR",
  "TMP",
  "TEMP",
  "LANG",
  "LC_ALL",
  "LC_CTYPE",
  "TERM",
  "USER",
  "LOGNAME",
  // Windows essentials.
  "SystemRoot",
  "SYSTEMROOT",
  "windir",
  "USERPROFILE",
  "APPDATA",
  "LOCALAPPDATA",
  "ProgramData",
  "ProgramFiles",
  "ComSpec",
  "PATHEXT",
  "NUMBER_OF_PROCESSORS",
];

function inheritedSidecarEnv(): Record<string, string> {
  const out: Record<string, string> = {};
  for (const key of SIDECAR_ENV_ALLOWLIST) {
    const v = Deno.env.get(key);
    if (v !== undefined) out[key] = v;
  }
  return out;
}

const GRACEFUL_SHUTDOWN_MS = 5_000;
const DEFAULT_STARTUP_TIMEOUT_MS = 30_000;
const RECENT_LOG_LINES = 10;

interface Active {
  startId: number;
  proc: Deno.ChildProcess;
  pid: number;
  abort: AbortController; // aborted on supersede/stop/unexpected exit
  recentLogs: string[];
  // Set true once the stdout readiness marker is seen; runReadiness polls it.
  markerSeen: boolean;
  // Set true by watchExit the moment the process is gone, so terminateActive
  // skips the kill (on Windows, killing an exited pid fails with EPERM noise).
  exited: boolean;
}

// Crash-after-ready flap guard. `RestartPolicy.maxAttempts` only bounds
// CONSECUTIVE readiness failures within one restartWithBackoff loop; a sidecar
// that boots healthy and THEN crashes starts a fresh loop each time, so without
// this it would restart forever (reloading a multi-GB model every ~1s). We track
// restart timestamps in a rolling window at the Sidecar level and give up if it
// flaps too often; the window naturally ages out so an occasional crash recovers.
const FLAP_WINDOW_MS = 60_000;
const FLAP_MAX_RESTARTS = 5;

class Sidecar {
  readonly kind: SidecarKind;
  private startId = 0;
  private active: Active | null = null;
  private current: SidecarSnapshot;
  private pendingStop = false;
  // Timestamps of recent unexpected-exit restarts (rolling FLAP_WINDOW_MS).
  private restartTimes: number[] = [];
  // Bumped on every EXPLICIT lifecycle call (start/stop, incl. restart, and
  // the flap-guard give-up). The internal crash-restart loop captures it and
  // exits when it changes, so a stale loop can never re-spawn with old options
  // over a newer explicit decision.
  private explicitGeneration = 0;
  // The single in-flight crash-restart loop (null when none). watchExit spawns
  // at most one; further crashes while it runs only feed the flap window and
  // are sequenced by the loop's own attempt cycle. Without this, every crash
  // spawned another concurrent loop and they piled up, spawning overlapping
  // processes.
  private restartLoop: Promise<void> | null = null;

  constructor(
    kind: SidecarKind,
    private readonly notify: (snap: SidecarSnapshot) => void,
  ) {
    this.kind = kind;
    this.current = { kind, status: "Disabled" };
  }

  snapshot(): SidecarSnapshot {
    return this.current;
  }

  private emit(next: SidecarSnapshot): void {
    this.current = next;
    this.notify(next);
  }

  async start(options: StartOptions): Promise<void> {
    // Explicit start: invalidate any in-flight crash-restart loop (its options
    // are stale relative to this call).
    this.explicitGeneration += 1;
    await this.startInternal(options);
  }

  private async startInternal(options: StartOptions): Promise<void> {
    // 1. Supersede any in-flight start. Null out this.active BEFORE awaiting
    //    terminate so the old watchExit's isCurrent check returns false the
    //    moment the old proc dies (otherwise we briefly emit a spurious Error
    //    while terminateActive is still in its post-race cleanup).
    this.startId += 1;
    const myStartId = this.startId;
    // Clear any stale stop intent from a prior stop(): a fresh start means we
    // want this process supervised. Without this, a stop() -> start() sequence
    // leaves pendingStop=true and the NEXT unexpected crash is swallowed by
    // watchExit (treated as an intentional stop) instead of triggering a
    // restart. The reset in watchExit's stop branch never runs on the stop()
    // path because stop() nulls `active` first, so isCurrent() returns early.
    this.pendingStop = false;
    if (this.active) {
      const old = this.active;
      this.active = null;
      await this.terminateActive(old);
    }

    // 2. Validate readiness URL before doing anything else.
    if (options.readiness?.kind === "http") {
      try {
        validateHealthCheckUrl(options.readiness.url);
      } catch (err) {
        // A non-loopback readiness URL (e.g. a user-set non-loopback llm.host) is
        // a configuration error, not a crash: surface it as an Error snapshot the
        // UI can show, instead of throwing into the boot path's silent .catch.
        this.emit({
          kind: this.kind,
          status: "Error",
          message: `invalid readiness URL (must be loopback): ${errMessage(err)}`,
        });
        return;
      }
    }

    this.emit({ kind: this.kind, status: "Loading", message: "Starting…" });

    // 3. Build env. Workspace env + caller env + library-path env.
    const libEnv = options.libraryDir ? libraryEnvFor(options.libraryDir) : { env: {} };
    const env: Record<string, string> = {
      ...inheritedSidecarEnv(),
      ...libEnv.env,
      ...options.env,
    };
    const cwd = options.cwd ?? libEnv.cwd;

    // 4. Spawn.
    let proc: Deno.ChildProcess;
    try {
      proc = new Deno.Command(options.binary, {
        args: options.args,
        env,
        cwd,
        stdout: "piped",
        stderr: "piped",
        stdin: "null",
      }).spawn();
    } catch (err) {
      const msg = errMessage(err);
      log.error(`${this.kind}: spawn failed: ${msg}`);
      this.emit({
        kind: this.kind,
        status: "Error",
        message: `spawn failed: ${msg}`,
      });
      return;
    }

    log.info(`${this.kind}: starting (pid ${proc.pid}): ${options.binary}`);

    const abort = new AbortController();
    const active: Active = {
      startId: myStartId,
      proc,
      pid: proc.pid,
      abort,
      recentLogs: [],
      markerSeen: false,
      exited: false,
    };
    this.active = active;

    // Windows-only: register the PID with the kill-on-close job object so
    // a hard core crash doesn't leave the sidecar running. No-op elsewhere.
    trackSidecarPid(proc.pid);

    // 5. Wire output capture + monitor unexpected exit.
    void this.captureOutput(
      active,
      options.readiness?.kind === "stdout" ? (options.readiness.marker ?? "READY\n") : null,
    );
    void this.watchExit(active, options);

    // 6. Readiness probe.
    const readinessOk = await this.runReadiness(active, options);
    if (!this.isCurrent(myStartId)) return; // superseded mid-probe
    if (!readinessOk) {
      log.warn(`${this.kind}: readiness timed out; killing`);
      await this.terminateActive(active);
      if (this.isCurrent(myStartId)) {
        this.active = null;
        this.emit({
          kind: this.kind,
          status: "Error",
          message: "readiness timeout",
        });
      }
      return;
    }

    if (!this.isCurrent(myStartId)) return;
    log.info(`${this.kind}: running (pid ${active.pid})`);
    this.emit({ kind: this.kind, status: "Running", pid: active.pid });
  }

  async stop(): Promise<void> {
    this.pendingStop = true;
    // Explicit disable also invalidates any in-flight crash-restart loop.
    this.explicitGeneration += 1;
    // Explicit disable is a clean slate: a later re-enable gets a fresh flap
    // budget rather than inheriting crash history from before the stop.
    this.restartTimes = [];
    if (!this.active) {
      this.emit({ kind: this.kind, status: "Disabled" });
      return;
    }
    const active = this.active;
    this.active = null;
    this.startId += 1;
    await this.terminateActive(active);
    log.info(`${this.kind}: stopped`);
    this.emit({ kind: this.kind, status: "Disabled" });
  }

  async restart(options: StartOptions): Promise<void> {
    // An external restart is a deliberate re-application (settings change, manual
    // retry from the UI, the wedged-slot watchdog), so clear the crash flap-guard
    // window: its job is to bound the INTERNAL auto-restart loop, not an explicit
    // re-apply. The internal loop calls startInternal() directly, so it is
    // unaffected and still accumulates crashes toward the cap.
    this.restartTimes = [];
    await this.start(options);
  }

  private isCurrent(startId: number): boolean {
    return this.active?.startId === startId;
  }

  // Pipe stdout+stderr through a tee so we can both (a) detect a stdout
  // readiness marker on one branch and (b) keep a ring buffer of the last
  // 10 lines on the other for error context if the process dies.
  private async captureOutput(active: Active, marker: string | null): Promise<void> {
    const decoder = new TextDecoder();
    const pump = async (
      stream: ReadableStream<Uint8Array>,
      tag: "stdout" | "stderr",
    ): Promise<void> => {
      let buf = "";
      try {
        for await (const chunk of stream) {
          buf += decoder.decode(chunk, { stream: true });
          const lines = buf.split("\n");
          buf = lines.pop() ?? "";
          for (const line of lines) {
            const trimmed = line.trim();
            if (trimmed.length === 0) continue;
            active.recentLogs.push(trimmed);
            if (active.recentLogs.length > RECENT_LOG_LINES) {
              active.recentLogs.shift();
            }
            // DEBUG on a dependency-gated scope (see DEPENDENCY_SCOPE_LEVELS
            // in shared/log.ts): the binaries' chatter stays out of the logs;
            // crash context still surfaces via the recentLogs ring above.
            log.debug(`[${this.kind}/${tag}] ${trimmed}`);
            if (marker && trimmed.includes(marker)) {
              active.markerSeen = true;
            }
          }
        }
      } catch {
        // Stream closed; expected on process exit.
      }
    };
    await Promise.all([pump(active.proc.stdout, "stdout"), pump(active.proc.stderr, "stderr")]);
  }

  private async runReadiness(active: Active, options: StartOptions): Promise<boolean> {
    const readiness = options.readiness ?? { kind: "warmup", ms: STARTUP_WARMUP_MS };
    const startupTimeoutMs = options.startupTimeoutMs ?? DEFAULT_STARTUP_TIMEOUT_MS;

    if (readiness.kind === "warmup") {
      try {
        await sleep(readiness.ms, active.abort.signal);
        return true;
      } catch {
        return false;
      }
    }
    if (readiness.kind === "http") {
      return await pollHttpHealth(readiness.url, {
        signal: active.abort.signal,
        attempts: Math.ceil(startupTimeoutMs / 1_000),
        intervalMs: 1_000,
      });
    }
    if (readiness.kind === "stdout") {
      // captureOutput already watches for readiness.marker (defaulted in
      // start()) and flips _markerSeen on the active entry; we just poll it.
      const deadline = Date.now() + startupTimeoutMs;
      while (Date.now() < deadline) {
        if (active.abort.signal.aborted) return false;
        if (active.markerSeen) {
          return true;
        }
        try {
          await sleep(100, active.abort.signal);
        } catch {
          return false;
        }
      }
      return false;
    }
    return false;
  }

  private async watchExit(active: Active, options: StartOptions): Promise<void> {
    const status = await active.proc.status;
    active.exited = true;
    // If this active was superseded or stopped intentionally, ignore.
    if (!this.isCurrent(active.startId)) return;
    if (this.pendingStop) {
      this.pendingStop = false;
      return;
    }
    // Unexpected exit. Abort the readiness probe first: startInternal may still
    // be polling health for this pid, and without the abort it would block its
    // caller for the full startup window (minutes for a large model) against a
    // process that is already gone.
    active.abort.abort();
    const msg =
      active.recentLogs.length > 0
        ? active.recentLogs.join("\n")
        : `exited with code ${status.code}${status.signal ? ` (signal ${status.signal})` : ""}`;
    log.warn(`${this.kind}: unexpected exit: ${msg}`);
    this.active = null;
    this.emit({ kind: this.kind, status: "Error", message: msg });

    // Restart with backoff if policy allows.
    const policy =
      options.restartPolicy === "none" ? null : (options.restartPolicy ?? DEFAULT_RESTART_POLICY);
    if (!policy) return;

    // Flap guard: a sidecar that keeps crashing shortly after becoming Running
    // must not restart indefinitely. Count this restart in the rolling window
    // and give up (terminal Error, no restart) if it exceeds the cap. The
    // generation bump makes an in-flight restart loop exit too.
    const now = Date.now();
    this.restartTimes = this.restartTimes.filter((t) => now - t < FLAP_WINDOW_MS);
    this.restartTimes.push(now);
    if (this.restartTimes.length > FLAP_MAX_RESTARTS) {
      log.error(
        `${this.kind}: flapping (${this.restartTimes.length} crashes in ` +
          `${FLAP_WINDOW_MS / 1000}s); giving up until re-enabled`,
      );
      this.explicitGeneration += 1;
      this.emit({
        kind: this.kind,
        status: "Error",
        message:
          `gave up after repeated crashes ` +
          `(${this.restartTimes.length} in ${FLAP_WINDOW_MS / 1000}s)`,
      });
      return;
    }
    // One loop drives recovery: if it is already running, this crash was one of
    // its attempts (counted above) and its status check sequences the next one.
    if (this.restartLoop) return;
    const gen = this.explicitGeneration;
    this.restartLoop = this.restartWithBackoff(options, policy, gen).finally(() => {
      this.restartLoop = null;
    });
    await this.restartLoop;
  }

  private async restartWithBackoff(
    options: StartOptions,
    policy: RestartPolicy,
    gen: number,
  ): Promise<void> {
    let delay = policy.initialDelayMs;
    for (let attempt = 1; attempt <= policy.maxAttempts; attempt++) {
      log.info(`${this.kind}: restart attempt ${attempt}/${policy.maxAttempts} in ${delay}ms`);
      await sleep(delay);
      // A newer explicit start/stop/restart owns the sidecar now (or the flap
      // guard gave up): this loop's options are stale, so it must bow out.
      if (this.pendingStop || gen !== this.explicitGeneration) return;
      await this.startInternal(options);
      if (gen !== this.explicitGeneration) return;
      if (this.current.status === "Running") return;
      delay = Math.min(delay * 2, policy.maxDelayMs);
    }
    log.error(`${this.kind}: gave up restarting after ${policy.maxAttempts} attempts`);
  }

  private async terminateActive(active: Active): Promise<void> {
    active.abort.abort();
    try {
      if (active.exited) {
        // Nothing to kill; fall through to the status drain below.
      } else if (Deno.build.os === "windows") {
        // No SIGTERM on Windows; SIGKILL is the only option via Deno.
        active.proc.kill("SIGKILL");
      } else {
        active.proc.kill("SIGTERM");
        // Race the grace period against the natural exit.
        const result = await Promise.race([
          active.proc.status.then(() => "exited" as const),
          sleep(GRACEFUL_SHUTDOWN_MS).then(() => "timeout" as const),
        ]);
        if (result === "timeout") {
          try {
            active.proc.kill("SIGKILL");
          } catch {
            // Already dead.
          }
        }
      }
    } catch (err) {
      log.warn(`${this.kind}: terminate failed: ${errMessage(err)}`);
    }
    // Drain status to release resources.
    try {
      await active.proc.status;
    } catch {
      // ignore
    }
  }
}

export class SidecarManager {
  private readonly sidecars = new Map<SidecarKind, Sidecar>();
  private readonly listeners = new Set<StatusListener>();

  private getOrCreate(kind: SidecarKind): Sidecar {
    let s = this.sidecars.get(kind);
    if (!s) {
      s = new Sidecar(kind, (snap) => this.broadcast(snap));
      this.sidecars.set(kind, s);
    }
    return s;
  }

  async start(kind: SidecarKind, options: StartOptions): Promise<void> {
    await this.getOrCreate(kind).start(options);
  }

  async stop(kind: SidecarKind): Promise<void> {
    const s = this.sidecars.get(kind);
    if (s) await s.stop();
  }

  async restart(kind: SidecarKind, options: StartOptions): Promise<void> {
    await this.getOrCreate(kind).restart(options);
  }

  status(kind: SidecarKind): SidecarSnapshot {
    return this.sidecars.get(kind)?.snapshot() ?? { kind, status: "Disabled" };
  }

  getStatuses(): SidecarSnapshot[] {
    return Array.from(this.sidecars.values(), (s) => s.snapshot());
  }

  subscribe(listener: StatusListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  // Called once on process exit. Stops every live sidecar concurrently.
  async shutdown(): Promise<void> {
    await Promise.all(Array.from(this.sidecars.values(), (s) => s.stop()));
  }

  private broadcast(snap: SidecarSnapshot): void {
    for (const l of this.listeners) {
      try {
        l(snap);
      } catch (err) {
        log.warn(`sidecar listener threw: ${errMessage(err)}`);
      }
    }
  }
}

// Singleton accessor. The rest of core wires routes/WS into this one.
let _instance: SidecarManager | null = null;
export function sidecarManager(): SidecarManager {
  if (!_instance) _instance = new SidecarManager();
  return _instance;
}

// Test-only: drops the cached instance so the next `sidecarManager()` call
// rebuilds against a fresh listener set.
export function __resetForTesting(): void {
  _instance = null;
}
