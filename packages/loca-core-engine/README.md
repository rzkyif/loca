# @loca/core-engine

The runtime-agnostic heart of loca-core. It owns the portable, stateful logic
(sessions/messages, chat orchestration + LLM streaming, settings, memories,
embedding/relevance, external STT/TTS, remote MCP) and exposes it behind a `Host`
abstraction, so the exact same TypeScript runs in two places:

- inside the Deno service (`@loca/core`), which supplies a `DenoHost` (Deno FS,
  `@db/sqlite`, the OS keychain) and serves the engine over `Deno.serve`; and
- in a future pass, inside the mobile Client's webview, which will supply a
  webview host (Tauri FS, an in-webview SQLite, platform secure storage) and drive
  the engine in process.

## The boundary

The engine imports **no** `Deno.*`, `@db/sqlite`, `@tauri-apps/*`, or `node:*`,
and no subprocess/serve APIs. That purity is enforced by the `loca/no-host-import`
oxlint rule (see [`.oxlintrc.json`](../../.oxlintrc.json) +
[`oxlint-plugin.ts`](../../scripts/lint-plugins/oxlint-plugin.ts)). Everything
runtime-specific is reached through the [`Host`](src/host.ts) the embedder injects
at [`init(host)`](src/engine.ts):

- Primitives: `Host.fs` (async), `Host.openDb` (a synchronous SQLite subset),
  `Host.secureStore` (the vault master key), `Host.config` (env/config),
  `Host.log`, `Host.capabilities`.
- Capability-gated providers (optional; a host omits what it can't do): `localEndpoints`
  (local llama/embed ports + local STT/TTS + speech-sidecar status + TTS voices),
  `tools` (extension + MCP tool catalog and execution), `status` (idle/telemetry +
  scheduled prompts + memory indexing), and `mcp` (remote-MCP server admin: registry
  CRUD, connection reconcile, OAuth). A mobile host supplies a reduced set (e.g. no
  `localEndpoints`), and the engine degrades where a provider is absent.
- The embedder drives an [`EngineInstance`](src/engine.ts): `handleHttp(req)` serves
  the engine's app-domain slice of `/api/v1/*` (a runtime-agnostic Hono `app.fetch`
  over the routes in [`src/http/`](src/http/): sessions, settings, memories, llm,
  stt, tts, mcp), `isEngineRoute(path)` tells the shell which paths to dispatch here,
  and `connect(clientId)` gives WS-equivalent frame exchange over the shared
  [`FrameBus`](src/frame-bus.ts). Auth is injected: the shell passes a bearer ->
  client `resolveClient` at `init`, so the engine never imports its authService.
  Frame payloads are the `@loca/shared` frame unions, so the wire contract matches
  the Client byte-for-byte.

Transport, TLS, pairing/auth, and every subprocess/native subsystem (sidecars,
binaries, self-update, the tool-worker sandbox, stdio MCP, helper binaries) stay
in `@loca/core`, along with the OS-bound routes (health, pairing, admin, models,
binaries, requirements, update, sidecars-status, storage, scheduled-prompts,
greetings, extensions, tools) it serves from its own Hono app.

## Run / check / test

Standardized verbs (fanned out by `scripts/pkg.ts` from the root, or run in-dir):

```
deno task --cwd packages/loca-core-engine check
deno task --cwd packages/loca-core-engine test
```

Tests are co-located as `*.test.ts`, following the repo convention.
