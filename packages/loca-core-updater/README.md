# loca-core-updater

Standalone Rust crate compiled to its own small binary (not a `deno compile`
entry), so the binary boundary is explicit and the artifact stays a few hundred
KB. It finishes a self-update from a process outside core. Core swaps the binary
in place itself (a running binary can be renamed on both Unix and Windows), so
the updater's job is normally just the restart: on Windows it starts the Task
Scheduler task once core has exited; in the no-service background mode it respawns
core directly. It still owns the swap as a fallback (pass `--staged`) for the
rare case core's in-place swap failed. On Unix with a supervisor the supervisor
restarts core, so the updater is not involved at all. The header comment in
[`src/main.rs`](src/main.rs) is the canonical deep doc; this README stays short.

## Layout

A single [`src/main.rs`](src/main.rs) with co-located unit tests.

## Usage

```
loca-core-updater --current <path> [--staged <path>] [--restart-args <json>]
                   [--supervisor schtask --service-label <task>]
```

Waits 2 seconds for the parent core process to exit. If `--staged` is given
(Windows), atomically installs it over `--current` while preserving the previous
binary as `<name>.old` (core's boot-time rollback restores it if the new binary
crash-loops, and deletes it once the update is committed); absent, core already
swapped the binary and the updater only restarts. It then restarts core:
`--supervisor schtask` starts the `--service-label` Task Scheduler task (so the
supervisor owns the new instance), otherwise it spawns core directly with
`--restart-args` forwarded as its argv. Exit codes: 0 success, 2 bad arguments
(missing `--current`), 3 Windows move-aside failed, 4 install rename failed, 5
restart failed (spawn or task start), 6 rollback-anchor creation failed (Unix;
nothing changed, safe to re-run). Logs to a per-channel `logs/updater.log`,
mirroring WARN/ERROR to stderr.

## Run, build, test

This crate exposes the standardized verbs as cargo wrappers (its `deno.json`):
`deno task lint:core-updater`, `test:core-updater`, `build:core-updater`, or
`cd` in and run `deno task <verb>`. It is also compiled as part of
`deno task build:core` and folded into the repo-wide `deno task lint` / `test`.

Invoked by core's self-updater; see
[`../loca-core/src/update/README.md`](../loca-core/src/update/README.md).
