# Headless integration E2E

This lane drives the **real Svelte app** in a **real Chromium** (vitest browser
mode + Playwright) talking over real HTTP+WS+TLS to a **real `loca-core`
subprocess**, with every outbound dependency (LLM, STT, TTS, model/binary
downloads) mocked locally. It is the fast, deterministic, cross-platform
counterpart to the `tauri-driver` lane (`../tauri-driver/`), and is where the
bulk of happy-path coverage lives.

It is **opt-in** (its own npm project, not in the workspace `deno.json`) and
**never runs in CI**, matching the `tauri-driver` lane.

## One-time setup

```sh
cd tests/e2e/headless
npm install
npx playwright install chromium
```

You also need a dev install present once (`deno task dev` at least once), which
stages the four helper binaries plus `deno` and `llama-server` under
`~/.loca/dev/core/bin`, and the Rust helpers built into `target/debug`. The
harness symlinks these into each throwaway test core so its boot-time checks
pass and the chat composer enables.

## Run

```sh
deno task test:e2e:headless                 # from the repo root
# or, from this dir:
npx vitest run
LOCA_E2E_CORE_LOGS=1 npx vitest run        # surface the spawned core's logs
```

## How it works

```
 vitest (Node)                              browser (Chromium, ignoreHTTPSErrors)
 ┌───────────────────────────┐              ┌──────────────────────────────────┐
 │ commands.ts  ──launchCore──┼──spawns────► │  real +page.svelte app shell      │
 │   core-process.ts (deno)  │              │  (mounted via E2eApp wrapper)     │
 │   mock-services.ts        │◄──HTTP/WS────┤  platform-e2e.ts: net -> real     │
 │   (LLM/STT/TTS/artifacts) │   over TLS    │  fetch/WebSocket; natives stubbed │
 └───────────────────────────┘              └──────────────────────────────────┘
```

- **`harness/core-process.ts`** spawns a `loca-core` subprocess in a throwaway
  `LOCA_CORE_HOME` tempdir on the `dev` channel (in-memory catalog, no signed
  fetch), seeds settings pointing LLM/STT at the mock and TTS off (so no native
  sidecars spawn), symlinks the required + sidecar binaries, points the test
  models dir at the shared real cache, and computes the core's real SPKI pin.
- **`harness/mock-services.ts`** is one HTTP server speaking OpenAI-compatible
  `/v1/chat/completions` (scriptable), `/v1/audio/transcriptions`,
  `/v1/audio/speech`, plus a HuggingFace/storage artifact host.
- **`harness/commands.ts`** registers vitest browser commands so an in-browser
  test can spawn/stop cores and inspect the core filesystem.
- **`harness/platform-e2e.ts`** is the test `Platform`: `net` makes real browser
  fetch/WebSocket calls to the core; everything else is stubbed/in-memory. The
  `capturePin` path returns the core's real SPKI pin so the pairing PAKE
  (which channel-binds the cert pin) succeeds.
- **`harness/E2eApp.svelte`** mirrors `+layout.svelte`'s shared-UI context setup
  (minus the Tauri platform install) and mounts the real `+page.svelte`.
- **`harness/app.ts`** (`launchApp`) ties it together and exposes page objects.

A spec reads at a high level:

```ts
const app = await launchApp({ scenario: "paired", llm: { kind: "text", text: "hi" } });
await app.chat.send("hello");
await app.chat.expectText("hi");
```

## Behaviour delta versus the `tauri-driver` lane

This lane exercises the full **product** behaviour of client <-> core: the entire
Svelte UI and all stores; the real `lib/core/` client stack (CPace pairing, REST,
WS streaming, reconnect, multiplexing); real HTTP+WS over real TLS to a real
core; and real core internals (SQLite, chat + LLM streaming, tool execution,
sidecar gating, settings, downloads, STT/TTS routes).

It deliberately does **not** exercise, and these remain covered by Rust unit
tests + the `tauri-driver` smoke lane + manual `deno task dev`:

- the Rust `net` transport (reqwest/rustls **SPKI pinning**) - replaced here by
  the browser's fetch/WebSocket (TLS is real; pin verification is the Rust
  wrapper's job);
- the native WebView engine (WKWebView/WebView2) vs Chromium;
- OS-native calls (global shortcuts, window management, screen/region capture,
  keychain, file picker) - stubbed by the test platform;
- the **unauthorized / "Re-pair needed"** state: the client only enters it when
  the WS handshake error carries `HTTP 401`/`403` (`client.ts` `isAuthRejection`),
  and the **browser WebSocket API hides the upgrade status**, so a revoked token
  reads as an ordinary drop. Covered by the Rust net unit tests + tauri-driver;
- **end-to-end VAD/mic capture**: `@ricky0123/vad-web` (Silero) loads its model
  from `baseAssetPath: "/vad/"` (not served by the headless vite root) and needs
  a real speech sample to fire, so a non-flaky end-to-end VAD spec isn't viable
  here. The loca-owned STT seam (the transcription request -> external mock) is
  covered in `stt.test.ts`; VAD firing is left to manual `deno task dev`.

## Phase 0 feasibility (proven)

The novel risks were validated before building coverage:

- A test core boots in a tempdir over self-signed TLS and serves `/health`.
- Chromium performs cross-origin `fetch` to the core over that TLS via the
  Playwright context's `ignoreHTTPSErrors`. **No core change was needed for
  CORS**: core's existing allowlist already permits loopback origins, which is
  where vitest serves the test page.
- The pairing PAKE channel-binds the cert SPKI pin. The harness computes the
  core's real pin Node-side (Chromium hides the cert) and feeds it through the
  `capturePin` path, so the real handshake completes - a genuine end-to-end
  client<->core pairing, not a client-only mock.

## Coverage status

- **Tier 1** (`tier1/`): pairing (fresh-lock + real PAKE), chat (send/stream,
  echo, multi-turn), navigation (all modes, no render error), sessions
  (new/list), model-downloader (pending -> download via mock -> chat enables),
  stt (client transcription chain via the external mock), **reconnect** (kill
  the core mid-session -> "Reconnecting" -> bring it back on the same pin ->
  resume), **errors** (wrong-code PAKE rejection; provider error mid-stream ->
  error bubble -> recovery).
- **Tier 2** (`tier2/`): appearance (theme/text-size -> DOM), general (user name
  reaches the model context), prompts (custom system prompt sent), snippets
  (create -> list), memories (create -> list), scheduled-prompts (create -> run
  -> automated session), greetings (enabled -> greeting session on launch),
  extensions (install the hermetic `fixtures/test-extension` -> listed),
  tool-calling (scripted call -> real deno worker executes the test tool ->
  final turn; plus a multi-tool turn), mcp (connect the hermetic
  `fixtures/mcp-server.ts` stdio server -> tool discovered -> **called over
  stdio**), tts (external provider round-trip), settings (core setting persists),
  **multi-core** (pair + switch two cores; per-core session isolation),
  **streaming** (interrupt a long stream -> partial persisted; a reasoning turn),
  **security** (a client can't read another client's session), **scenario-seed**
  (declarative `seed.sessions` pre-mount state).

Deeper MCP/extension lifecycle (resources/prompts, a second server, extension
uninstall/update) and the self-update flow (mock signed manifest -> staged swap)
are sensible future depth, not yet built.

Two feature areas are covered at the unit layer rather than here, by design:

- **dual-model routing**: the secondary-endpoint resolution is unit-tested
  (`loca-core/src/services/endpoint-resolver.test.ts`). The complexity
  classifier that auto-picks a route only runs for turns carrying NO route, but
  the WS schema defaults a frame's route to `"default"` and every current caller
  pins one, so the classifier is not reachable from the client boundary.
- **sad paths**: external STT/TTS misconfig + upstream errors
  (`stt-transcribe.test.ts`, `tts-synthesize.test.ts`), CORS on raw-Response
  endpoints (`http/middleware/cors.test.ts`), and the STT transcription-chain
  failure paths (`lib/stt/transcription.test.ts`) live in co-located unit tests.

## Writing a spec

- Put permanent specs under `specs/tier1/` (core flows) or `specs/tier2/`
  (configurable features). Scratch specs are `*.tmp.test.ts` (gitignored).
- Happy paths, plus wire-only negatives. Default to happy paths; the one
  exception is a negative that only emerges from the real client<->core wire and
  can't be faithfully unit-tested (wrong-code PAKE rejection, a provider error
  mid-stream, a dropped-and-recovered connection - see `specs/tier1/errors.test.ts`
  and `reconnect.test.ts`). Any sad path with a faithful seam stays in a
  co-located unit/component test.
- Prefer page objects (`harness/pages/*`) and existing `data-testid`s. New
  testids go on the shared `*View` component (single-source rule), not a client
  wrapper.
