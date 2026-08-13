# loca-core-ptyhost

Standalone Rust crate compiled to its own small binary (not a `deno compile`
entry), so the binary boundary is explicit and the artifact stays small. It
spawns a Deno tool worker with stdin and stderr attached to a pseudo-terminal (a
unix PTY, or a ConPTY on Windows), because Deno only shows its interactive
permission prompt when both of those streams are terminals. Core drives the
prompt through a small NDJSON control protocol on the helper's own stdio. The
header comment in [`src/main.rs`](src/main.rs) is the canonical deep doc; this
README stays short.

## Layout

A single [`src/main.rs`](src/main.rs) with co-located unit tests: a `unix`
module (nix `openpty`) and a `windows` module (portable-pty ConPTY), both behind
the same control protocol. The two paths differ in how the protocol reaches the
worker: see below.

## Protocol

Control frames arrive one JSON object per line on stdin: `spawn` (exactly once,
first; carries cmd, args, the child's entire env, optional cwd), `write`/`answer`
(base64 bytes for the terminal master: worker frames and prompt answers), and
`kill`. Events leave one JSON object per line on stderr: `pty` (base64 terminal
output: worker stderr plus Deno prompt text), `exit` (child exited; ptyhost
mirrors the code), and `fatal`. ptyhost never writes to its own stdout.

Where the worker protocol flows differs by platform:

- **unix**: the worker's stdout is inherited, so the core<->worker NDJSON stream
  passes through untouched. The PTY slave keeps ECHO on (Deno refuses to prompt
  in raw mode) but drops canonical mode (no line-length limit on large frames);
  the master reader cancels the echo of everything core writes so the stream
  stays clean.
- **windows**: a ConPTY merges and reflows the child's stdout, so the protocol
  cannot ride it. The worker instead connects back to core over a per-worker
  loopback socket (see core's `control-socket.ts`) and speaks the protocol
  there; the ConPTY carries only Deno's prompt. No echo cancellation is needed
  because nothing byte-exact flows through the pseudoconsole. ptyhost also plays
  the hosting terminal's side of the ConPTY session: it answers conhost's
  cursor-position query (`ESC[6n`, which otherwise blocks the whole session),
  translates `\n` in answers to `\r` (a console line read only completes on
  Enter), and closes the pseudoconsole when the child exits (ConPTY never EOFs
  its output pipe on its own).

## Run, build, test

This crate exposes the standardized verbs as cargo wrappers (its `deno.json`):
`deno task lint:core-ptyhost`, `test:core-ptyhost`, `build:core-ptyhost`, or
`cd` in and run `deno task <verb>`. It is also compiled as part of
`deno task build:core` and folded into the repo-wide `deno task lint` / `test`.
