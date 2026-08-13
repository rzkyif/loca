# Sidecars

Supervision for every server-style subprocess core spawns: `llama-server` (LLM
chat, plus a second instance for embeddings) and `loca-core-speech`
(Speech-to-Text + Text-to-Speech). [`manager.ts`](manager.ts) is the generic
supervisor; per-kind argument construction lives in [`llama.ts`](llama.ts),
[`llama-embed.ts`](llama-embed.ts), and [`speech.ts`](speech.ts). Binary path
resolution does NOT live here (see `src/binaries/`). The deno tool workers are
supervised separately (see `src/extensions/`).

The speech sidecar runs a single engine, so concurrent multi-client STT / TTS
requests are queued fairly by `services/speech-scheduler.ts` (per-client
round-robin, one slot, `server_busy` past a depth cap) before they reach
[`speech.ts`](speech.ts), mirroring `services/llm-scheduler.ts` for the LLM. Both
queues feed the aggregate core status (`services/core-status.ts`), which the core
broadcasts as `core.status` frames and surfaces on `/health`.

## Lifecycle and supersession ([`manager.ts`](manager.ts))

`SidecarManager` holds one `Sidecar` per kind and broadcasts every
`SidecarSnapshot` change to subscribers (the WS hub forwards them to clients).
Each start:

- Bumps a monotonic `startId`. An in-flight start is superseded, never raced:
  the old process is terminated, and any readiness probe still running for it
  bails out the moment `isCurrent()` turns false.
- Spawns with an allowlisted environment (`SIDECAR_ENV_ALLOWLIST`), never the
  core's full env, so operator secrets such as `GITHUB_TOKEN` are not inherited
  by third-party binaries.
- Transitions `Loading` -> `Running` on readiness, `Error` on spawn failure,
  readiness timeout, or unexpected exit. `stop()` lands on `Disabled`.
- Keeps a ring buffer of the last 10 stdout/stderr lines; on unexpected exit
  those lines become the `Error` snapshot message.
- Graceful kill: SIGTERM, a 5 s grace window, then SIGKILL on Unix; SIGKILL
  directly on Windows (Deno offers no SIGTERM there).
- Restarts after a crash with exponential backoff (defaults: 5 attempts, 1 s
  initial delay, doubled per attempt, capped at 30 s).

## Readiness ([`readiness.ts`](readiness.ts), [`types.ts`](types.ts))

Three `ReadinessCheck` modes: `http` (poll a URL until the first 2xx), `stdout`
(wait for a marker substring, default `READY\n`, on the child's output), and
`warmup` (sleep a fixed duration, then declare Running). HTTP health-check URLs
are validated to be loopback-only (`http://` to `127.0.0.1` or `localhost`);
external endpoints are rejected so sidecar supervision stays on the user's
machine. The stdout-marker scan itself lives in `manager.ts`'s output pump to
avoid teeing streams twice.

## Windows orphan cleanup ([`jobctl.ts`](jobctl.ts))

On Windows, core creates a Job Object with `KILL_ON_JOB_CLOSE` via
`kernel32.dll` FFI and assigns every spawned sidecar PID to it. Core holds the
only handle, so when core dies (gracefully or via `taskkill /F`) the kernel
terminates every assigned process. Windows-only by necessity: on Linux and macOS
the service supervisor already reaps orphans (systemd kills the unit's cgroup,
launchd kills the job's process group), while Windows Task Scheduler does
neither. The module no-ops cleanly on non-Windows.

## Library paths ([`library-path.ts`](library-path.ts))

`libraryEnvFor(dir)` prepends a directory to the platform's shared-library
search path: `DYLD_LIBRARY_PATH` on macOS, `LD_LIBRARY_PATH` on Linux, `PATH` on
Windows. On Windows it also overrides the child's cwd to that directory, which
is load-bearing: `ggml_backend_load_all()` scans the current path for
`ggml-*.dll` compute backends.

## Process metrics ([`process-metrics.ts`](process-metrics.ts))

Samples per-PID CPU% and RSS via `pidusage` for the Services settings field. A
PID that already exited is simply absent from the result map, so one dead
sidecar never blanks the batch. The `/sidecars/status` route picks the PIDs
itself (tracked sidecars plus the core process); client-supplied PIDs are never
probed.

## The worker deno binary ([`worker-deno.ts`](worker-deno.ts))

The sandboxed tool workers and the npm-based extension installer run as
`deno run` subprocesses using a bundled `deno` sidecar binary, which is a
downloadable requirement. `requireWorkerDeno()` resolves and existence-checks it
so callers get a clean `binary_not_found` error instead of a raw `NotFound` when
it is not installed yet.
