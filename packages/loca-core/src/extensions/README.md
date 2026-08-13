# Extensions (core side)

How core installs, registers, trusts, and executes extensions. The `loca.json`
format and everything a extension author needs live in
[the built-in extension README](../../../loca-extension-builtin/README.md); this document
only covers the host machinery.

An extension is one of two tool providers; the other is an MCP server, whose
host machinery lives in [`../mcp/README.md`](../mcp/README.md). Extensions also
provide memories: a `loca.json` may declare a `memories` array of bundled
knowledge and skills, installed read-only alongside the user's own.

## Two-phase install ([`installer.ts`](installer.ts))

Both phases are explicit user actions:

- **Download** (`startDownload`): acquire the files (fetch + extract an npm
  tarball, copy a local folder, or resolve the built-in's bytes), validate
  `loca.json`, and upsert extension/tool rows at status `downloaded`. The
  folder stays byte-identical to what was downloaded; `deno.json` is never
  edited and the content hash is left unpinned.
- **Install** (`startInstallDeps`): run `deno install` for declared deps (with
  `DENO_DIR=~/.loca/core/deno-cache` and `--node-modules-dir=auto`, so
  everything lands under `~/.loca/`), pin the content hash, and flip the row to
  status `installed`.

`startUpdate` chains both under one job, and the install path re-pins the hash
so a legitimate update never reads as drift. Callers pass an `InstallEventSink`
to receive `install_log` and `install_done` frames for forwarding to the
requesting client over WS. [`uninstall.ts`](uninstall.ts) is the inverse: delete
drops rows and files; uninstall reverts a deps-bearing extension to `downloaded`
by removing `node_modules` and `deno.lock` only.

## Discovery and registry

npm discovery ([`npm-registry.ts`](npm-registry.ts)) searches for packages
carrying the `loca-extension` keyword and resolves `package@version` to a
tarball URL; downloads are verified against npm's strong `dist.integrity` (SRI)
hash, not the legacy sha1 shasum, before extraction. The DB-backed registry
([`registry.ts`](registry.ts)) aggregates the `extensions`, `tools`,
`tool_embeddings`, and `grants` tables and owns drift handling:
`verifyHashFresh` re-checks a extension's content hash at tool-call time (cheap
via a stat-signature skip-cache), and on mismatch flips the extension to status
`drift`, disables all its tools, and throws; only an explicit user re-enable
clears it.

## Trust anchor ([`hash.ts`](hash.ts))

The content hash is a deterministic SHA-256 over the extension folder: walk,
exclude `node_modules` and the root `deno.lock` (generated after pinning, so
including them would read install churn as tampering), apply `.gitignore` rules
(the `.gitignore` file itself stays included), sort paths, and hash
`path\0size\0content` per file. The `deno.json` the deps derive from IS hashed,
so tampering with declared imports is still detected.

## Least-privilege execution

[`validate-args.ts`](validate-args.ts) first validates LLM-emitted arguments
against the tool's JSON-Schema `parameters` with Ajv (filling defaults and
stripping catastrophic-backtracking regexes before they can compile).
[`permissions.ts`](permissions.ts) then maps granted permission declarations to
exact Deno `--allow-*` flags, expanding path templates (`$home`, `$downloads`,
`$models`, `$sessions`, `$extension`, `$env.VAR`). Workers are keyed per
(extension, tool), so a process holds ONLY the invoked tool's grants, never a
sibling tool's net/run/ffi access. The pool ([`worker-pool.ts`](worker-pool.ts))
keeps workers warm with LRU eviction and an idle timeout (defaults: 8 warm
workers, 5 min idle, 60 s call timeout, paused while an `askUser` question is
pending). [`worker-handle.ts`](worker-handle.ts) owns one subprocess and its
NDJSON channel ([`worker-protocol.ts`](worker-protocol.ts)): frames cover boot,
call, cancel, progress, `askUser` request/response, log, and `stderr_log`;
stdout frames are capped at 16 MB and stderr lines at 1 MB so a runaway tool
cannot exhaust core's memory. The worker env is cleared and rebuilt from a small
operational allowlist plus explicitly granted env keys.

## Runtime permission prompts

Grants are three-state (`granted` | `ask` | `denied`; no row behaves as `ask`).
Only `granted` permissions become `--allow-*` spawn flags. Everything else
relies on Deno's interactive permission prompt at the moment of access: when the
[`loca-core-ptyhost`](../../../loca-core-ptyhost/README.md) helper binary is
present, [`worker-handle.ts`](worker-handle.ts) spawns the worker under it with
stdin + stderr on a pseudo-terminal (a unix PTY, or a ConPTY on Windows) and
WITHOUT `--no-prompt`, so an uncovered access pauses the op mid-call instead of
throwing. On Windows the ConPTY reflows the child's stdout, so the worker
protocol rides a per-worker loopback socket ([`control-socket.ts`](control-socket.ts))
instead of stdout, leaving the pseudoconsole for the prompt; the rest of the
flow (parser, matcher, WS frames) is identical to unix.

[`prompt-parser.ts`](prompt-parser.ts) (pure state machine, fixture-tested)
extracts the permission kind, resource, and API name from the prompt text on the
terminal; [`prompt-matcher.ts`](prompt-matcher.ts) decides: declared permission with
state `ask` (or no row) forwards to the user in chat over the
`tool.permission_request` / `tool.permission_response` WS frames (the pool
pauses the call-timeout budget exactly like `askUser`); declared + `denied` and
policy-denied undeclared accesses are auto-answered `n`; undeclared accesses
follow the extension's `undeclared_policy` column (`deny` default, `ask`
forwards flagged as undeclared). Unrecognized prompt kinds fail closed.

Answer timing matters: Deno flushes stdin until it has been quiescent for ~100
ms before reading the answer, so the handle writes `y\n`/`n\n` no sooner than
300 ms after the prompt appears and retries every 600 ms until the
`Granted`/`Denied` confirmation line settles (give-up after 10 s kills the
worker; the call then settles via the pool timeout). Accepts are scoped to the
current tool call: Deno caches verdicts per resource for the process lifetime,
so any worker whose prompt was user-answered is retired at call end instead of
returning to the warm pool.

The prompt wording is not a stable Deno API. The bundled deno is therefore
pinned on every channel (`pinnedTag` in `UPSTREAM_BINARIES`), and
[`prompt-live-probe.test.ts`](prompt-live-probe.test.ts) drives real prompts
through the real ptyhost + parser as the drift tripwire for deno bumps (unix
fixture, and the CI cover for the unix PTY path; the ConPTY path gets an
equivalent WorkerHandle-level live cover in
[`worker-handle-live.test.ts`](worker-handle-live.test.ts), which runs
Windows-only and locally since the CI test lane is Linux-only, plus the socket
transport test and the ConPTY-shaped parser fixture). Without the helper (a from-source dev setup that
has not built it), workers fall back to the legacy `--no-prompt` spawn and
ask-state permissions surface to the tool as `NotCapable`.

## Built-in extension seeding

The built-in extension is CDN-distributed (never on npm) behind an
Ed25519-signed manifest ([`seeded-manifest.ts`](seeded-manifest.ts)),
mirroring the binaries manifest; dev runs from the in-repo codebase instead. On
first boot, [`seeding.ts`](seeding.ts) installs it OFFLINE from the planted
artifacts (`extensions/.loca-extension-builtin.{tgz,json}`) - staged into the
native installer's payload at build time when reachable, otherwise fetched by
the `install-service` bootstrap during the install phase: it re-verifies the
manifest signature and the tarball sha256, then extracts. This honors the
no-non-consented-network rule (see [AGENTS.md](../../../../AGENTS.md)) - core
never fetches on boot; the install phase did the downloading. If the artifacts
are not planted, the
built-in is left for the user to install through the (user-gated) extensions UI,
which is the only path that hits the CDN. The seed leaves it at status
`downloaded` with no grants: Install, trusting, and per-tool Enable stay explicit
user steps. A sparse `extensions.builtinSeeded` core setting records the seed so
a user-deleted built-in does not come back on the next boot. Built-in updates are
surfaced only by the user-gated `POST /extensions/check-updates`, never by a
boot-time version check.
