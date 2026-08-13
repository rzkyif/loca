# AGENTS.md

_Keep this file at altitude: a high-level overview, common conventions, and
important decisions only. Specifics (build/test commands, the persistence tree,
release mechanics, extension details) belong in the docs linked under
"Pointers", not here. When tempted to add something, prefer the canonical doc
and link it._

## Project Overview

Tomat is a local-first modular AI client split into a long-running service and a
thin desktop UI that talk over an HTTP+WS API, plus helper binaries, a bundled
extension, and a distribution website. Tools come from two kinds of provider: an
**extension** (a `tomat.json` bundle the core installs) and an **MCP server** (a
Model Context Protocol process the core connects to). The packages:

- **`@tomat/core`**: Deno service that hosts the engine and owns everything
  runtime- or OS-bound: the HTTPS/WSS transport, TLS + multi-client pairing auth,
  sandboxed tool execution, MCP server connections, TTS/STT supervision, model +
  binary downloads, extension installation, and self-update. It supplies the
  engine a `DenoHost` (Deno FS, `@db/sqlite`, the OS keychain) and serves it over
  `Deno.serve`.
- **`@tomat/core-engine`**: the runtime-agnostic heart of core, extracted so the
  same TypeScript can run both in the Deno service and, in a future pass, inside
  the mobile Client's webview. Owns the portable stateful logic (session/message
  storage, LLM streaming, settings, memories + embedding-based relevance, external
  STT/TTS, remote MCP) behind a `Host` abstraction; imports no `Deno.*`,
  `@db/sqlite`, `@tauri-apps/*`, or `node:*` (enforced by `tomat/no-host-import`).
- **`@tomat/core-updater`**: standalone Rust binary (`tomat-core-updater`) that
  swaps in a staged core build during self-update, then restarts core.
- **`@tomat/core-keychain`**: standalone Rust binary (`tomat-core-keychain`)
  that reads/writes/deletes secrets in the OS keychain over a stdio protocol.
- **`@tomat/client`**: Tauri + Svelte desktop UI, intentionally "dumb": it
  renders, captures input, plays audio, manages global shortcuts, and talks to
  one or more paired cores.
- **`@tomat/shared`**: TypeScript types + Zod validators consumed by both sides
  (API contract, domain shapes, `tomat.json` schema, WS frame unions), plus the
  shared UI layer under `./ui/*` (design tokens, UnoCSS preset, and the
  presentational Svelte components the client and website both render). The
  non-UI exports stay side-effect-free; only `./ui/*` pulls in
  `svelte`/`unocss`, and core never imports it.
- **`@tomat/tomat-extension-builtin`**: reference/baseline extension installed by
  default on a fresh core; doubles as the worked example for the `tomat.json`
  format.
- **`@tomat/tomat-extension-samples`**: dev-only capability showcase exercising
  the full `tomat.json` surface (every askUser kind, display kinds, llm/tts/stt/db/schedulePrompt,
  a sample knowledge memory and a sample skill); NOT installed in production,
  only codebase-installed in the dev environment.
- **`@tomat/website`**: Astro static site at `au.tomat.ing`: the landing page,
  the feature showcase, and the user manual. Renders the shared
  `@tomat/shared/ui` components so demos match the app, and is multi-page (link
  navigation + view transitions, not an SPA) so navigation works without JS.

**Tech stack:** Deno 2, Hono, and SQLite (`jsr:@db/sqlite`) for the core; Tauri
2, Svelte 5, Vite, and UnoCSS for the client. Deno-first workspace: no
`package.json`, no bun, no pnpm; the client invokes Vite and the Tauri CLI
through `npm:` specifiers from `deno task`.

**Key locations:**

- Domain types, the API contract, and the `tomat.json` schema live in
  `packages/tomat-shared/src/`. When changing the wire format, update this
  package first.
- HTTP routes are split: the app-domain slice (sessions, settings, memories, llm,
  stt, tts, mcp) lives in `packages/tomat-core-engine/src/http/` and is served via
  `engine.handleHttp`; the OS-bound + transport routes (health, pairing, admin,
  models, binaries, requirements, update, sidecars, storage, scheduled-prompts,
  greetings, extensions, tools) stay in `packages/tomat-core/src/http/routes/`. The
  Deno shell's `main.ts` dispatches each request to whichever owns it. WS frames:
  `packages/tomat-shared/src/api/ws.ts`. Settings keys consumed by core are
  documented in the header of `packages/tomat-core-engine/src/services/chat.ts`.
- Client API access: `packages/tomat-client/src/ui/lib/core/`. Platform-specific
  Tauri calls: `packages/tomat-client/src/ui/lib/platform/`.
- Extension author API: the Zod schema in
  `packages/tomat-shared/src/validation/tomat-json.ts` and the published JSON
  Schema in `packages/tomat-shared/src/tomat-json-schema.json`.
- MCP server connections (the other tool provider): the subsystem under
  `packages/tomat-core/src/mcp/` (manager, registry, token resolution).

## Conventions

- **Tauri boundary (absolute rule):** nothing under
  `packages/tomat-client/src/ui/` outside `lib/platform/` may import from
  `@tauri-apps/*` (`tauri.ts` desktop, `mobile.ts` android, their `shared.ts`
  helpers, and the `select.ts` OS-based bootstrap). Add a method to the
  `Platform` interface in `lib/platform/index.ts`, implement it in `tauri.ts`
  and `mobile.ts`, cover it in the `src/ui/test/platform-stub.ts` fixture, then
  call `platform().<namespace>.<method>()`. There is no web client. Enforced by
  the `tomat/no-tauri-import` oxlint rule (`.ts`) and a `.svelte` grep pass, both
  in `deno task lint`.
- **Single-source UI (absolute rule):** every UI component the website renders
  must be the EXACT SAME shared `@tomat/shared/ui` component the client renders,
  at EVERY layer (primitive AND composition: a field control, a field row, a
  section, a group header, a sidebar, a whole panel). "Shared" means BOTH sides
  render it: the client wraps the shared component feeding live state; the
  website wraps the same component feeding scripted/default state. A shared
  component that ONLY the website uses is NOT allowed - it is a
  re-implementation in disguise and WILL drift (this is exactly how the settings
  demo diverged from the app). The fix for any represented-component mismatch is
  to push that layer into `@tomat/shared/ui` and make the client consume it,
  NEVER to hand-mirror the client's markup on the website. If extracting a layer
  breaks the client, that breakage is the next task to fix (by making the client
  wrap the shared component); it is never acceptable to leave a
  client-vs-website divergence standing. Client-only behavior on a shared
  component (validation, pickers, capture, live catalogs) is injected via
  props/callbacks/snippets, so the rendered markup stays single-source. The
  mechanism is a four-tier component taxonomy (A0 primitive / A `*View` / B thin
  client wrapper / C client shell) recorded in
  `packages/tomat-client/src/ui/components/.tiers.json`, sample bundles + a
  website gallery, and three lint walkers; the canonical reference is
  [shared UI README](packages/tomat-shared/src/ui/README.md). See also
  [website AGENTS.md](packages/tomat-website/AGENTS.md).
- **File naming** follows each host language's idiom: TypeScript `kebab-case`,
  Svelte components `PascalCase`, Rust `snake_case`, folders `kebab-case`,
  `scripts/` `kebab-case`. A `*.test.ts` next to a `.svelte` component mirrors
  the component name (`Toggle.test.ts`); otherwise tests follow the TypeScript
  rule. Identifiers in code follow `camelCase`/`PascalCase` regardless of
  filename. Never rename a setting `id` string because a file was renamed: those
  strings are persisted on disk and on the wire.
- **Commit messages** are a single lowercase line, `type: summary`, with NO
  scope: write `fix: force-local tar on windows`, never `fix(release): ...`. The
  types in use are `feat`, `fix`, and `bump`. No body, no scope parentheses, no
  trailers.
- **No em dashes.** `deno task lint` rejects the em dash (U+2014) across the
  whole repo (code, comments, strings, Markdown, and every other tracked file).
  When flagged, reword so the sentence reads naturally rather than swapping in
  another symbol.
- **Brand is Tomat.** The product is written **Tomat**, capitalized like any
  proper noun, in prose, headings, and user-facing copy. The two halves are
  **Core** (the service) and **Client** (the desktop UI), capitalized when named
  in a sentence, whether on their own (preferred: "Core", "Client") or with the
  brand ("Tomat Core", "Tomat Client"). The thin vocabulary: `Tomat` (the
  product), `Tomat Core` / `Core` (the service), `Tomat Client` / `Client` (the
  desktop UI), and `Tomat built-in extension`. Identifiers keep their own casing
  and stay lowercase: hyphenated package, crate, and binary names
  (`tomat-core`), the `@tomat/*` import scope, the all-caps `TOMAT_*` env vars,
  `au.tomat.ing` and the bundle ids derived from it, the `~/.tomat` paths, the
  `tomat.json` manifest filename, release artifact filenames (`tomat.pkg`), CSS
  classes (`tomat-scroll`), and the `tomat::<scope>` log targets.
- **Logging.** `tomat-core` modules log via `getLogger("scope")` from
  `src/shared/log.ts`; `console.*` is forbidden except the boot-failure catch in
  `main.ts`. Every line runs through `scrubSecrets` (same module) which masks
  tokens before they reach `core.log` or stderr.
- **Icons.** UI icons are UnoCSS `presetIcons` classes (`i-<collection>-<name>`,
  almost always `i-material-symbols-*`). A missing icon emits no CSS and renders
  as an invisible box with no build error, so every icon name is validated
  against the bundled `@iconify/json` by the `check-icon-classes.ts` walker in
  `deno task lint`. Verify a name exists (e.g. at https://icones.js.org) before
  using it; not every icon has a `-rounded`/`-outline` variant.
- **Library reuse.** Prefer maintained npm/JSR packages over hand-rolled code.
- **User-facing copy.** Every string a user can read (settings, manual, tool and
  extension descriptions, messages) follows the one guide at [COPY.md](COPY.md):
  voice, the canonical terminology glossary, and the per-surface rules. Read it
  before writing or editing any user-facing copy, in any package.

## Key Decisions

- **No non-consented network (absolute rule).** Once installed, the running Core
  and Client make NO outbound network request without an explicit user action:
  update checks happen on a button press, downloads behind a confirmation modal
  (see `downloads/manager.ts` + the requirements/download routes + the client's
  `requestRequiredModal`). Boot and background code must not fetch, poll, check
  for updates, or download. The built-in extension is installed on first boot
  OFFLINE from artifacts fetched + verified during install (the installer
  payload carries `extensions/.tomat-extension-builtin.{tgz,json}` when the
  build could stage it, and the Core binary's `bootstrap`/`install-service`
  subcommand - which every native installer's install hook wraps - plants them
  otherwise; `seeding.ts` then re-verifies the Ed25519 signature + tarball
  sha256 and installs them with zero network) - never a boot-time fetch. If the
  plant is missing (an offline installer run), boot leaves the built-in for
  the user to install from the extensions UI rather than fetching.
  The ONLY place network is expected without a user action is the install phase
  itself - the `install-service` subcommand the native installers delegate to -
  which runs before the app is considered installed. New boot/background work
  routes through the user-gated download and update flows; it never reaches out
  on its own.
- **Channels and persistence.** All state lives under `~/.tomat/<channel>/`,
  where the channel (`stable` default, `dev`, `latest`) is selected by
  `TOMAT_CHANNEL` and resolved identically in core, client, and the native
  installers. The one exception is `models/`, shared across channels at
  `~/.tomat/models` so multi-GB weights are not re-downloaded per channel.
  Keychain entries, default ports, and service labels are channel-namespaced so
  two channels can run at once. See [DEVELOPMENT.md](DEVELOPMENT.md) for the
  channel/port tables.
- **Trust root.** Release manifests are Ed25519-signed. The private key lives in
  a gitignored `.env`; the public key is committed in
  `packages/tomat-core/data/signing-keys.json` so every compiled core trusts the
  matching signatures. Three signing identities are in play: (1) the Ed25519
  keypair signs `core.json`, `binaries.json`, and `android.json` (the Android
  client also verifies `android.json.sig` against the committed public key before
  self-updating); (2) a separate Tauri-format keypair signs `client.json` and the
  bundled desktop installers; (3) a Java keystore signs the Android APK for
  install (Android enforces it at install time). The keystore is supplied at
  release time via base64 in the gitignored `.env`
  (`TOMAT_ANDROID_KEYSTORE_B64`); the decoded `*.jks` + `keystore.properties` are
  gitignored and never committed. A fourth, OPTIONAL identity pair applies to macOS
  (via the `APPLE_*` vars in `.env`): an Apple Developer ID Application
  certificate signs + notarizes the macOS client bundle,
  and a Developer ID Installer certificate signs + notarizes the macOS `.pkg`
  installer (`productsign` rejects the Application cert). Both are inert when unset - the
  build falls back to ad-hoc signing (`signingIdentity` "-") and the `.pkg` ships
  unsigned, so Gatekeeper warns until the user clears it once. The
  same `APPLE_*` env (its App Store Connect API key) also gates + signs the iOS
  App Store build, purely secret-driven. A fifth, also-dormant identity is
  Windows Authenticode (`WINDOWS_CERTIFICATE_THUMBPRINT` / `WINDOWS_SIGN_COMMAND`),
  which would sign the client NSIS + the unified installer; unset today, so
  Windows installers ship unsigned and SmartScreen warns on first run. See
  [scripts/release/macos-signing.md](scripts/release/macos-signing.md).
- **Distribution split.** Two Cloudflare-hosted hostnames, aligned with content
  type: `au.tomat.ing` (Astro Worker, landing page only) and `get.au.tomat.ing`
  (R2 public bucket, every release artifact: installers, schemas, manifests,
  binaries). Both are defined in `packages/tomat-core/src/config.ts`.
- **Helper-binary boundary.** The updater and keychain helpers are standalone
  Rust crates compiled to their own small binaries (not `deno compile` entries)
  so the binary boundary is explicit and the artifacts stay a few hundred KB.

## Pointers

For anything beyond the above, the canonical docs are:

- Build / run / test commands, channels: [DEVELOPMENT.md](DEVELOPMENT.md)
- Package vs release-item separation and the standardized per-package task
  vocabulary (`<verb>` / `<verb>:<pkg>`, fanned out by `scripts/pkg.ts`):
  [DEVELOPMENT.md](DEVELOPMENT.md). A package is a development unit (the root
  `deno.json` `workspace` array is the source of truth, 7 Deno + 5 Rust crates);
  a release item is a distribution unit that may span packages
  (`scripts/release/*.ts`, each declaring its `packages`).
- Per-package overview (layout, run/build/test, internals): the
  `packages/<name>/README.md` of the package in question. Subsystem deep dives
  are nested next to the code:
  `packages/tomat-core/src/{sidecars,extensions,mcp,update}/README.md` and
  `packages/tomat-client/src/ui/lib/core/README.md`.
- Test-suite guide (layout, helpers, fixtures, scratch tests, CI):
  [tests/README.md](tests/README.md). Two opt-in, local-only, never-in-CI E2E
  lanes sit alongside the co-located unit/component suites: a headless
  integration lane (real app in Chromium <-> real core over TLS, outbound deps
  mocked; the primary lane, happy paths only) and a tauri-driver smoke lane
  (native WebView + Rust transport). Deep dive:
  [tests/e2e/headless/README.md](tests/e2e/headless/README.md).
- Release + deploy, channels, Cloudflare + R2 setup:
  [packages/tomat-website/README.md](packages/tomat-website/README.md)
- macOS Developer ID signing + notarization (cert-ready checklist, entitlements,
  iOS notes): [scripts/release/macos-signing.md](scripts/release/macos-signing.md)
- Extension author API:
  [packages/tomat-extension-builtin/README.md](packages/tomat-extension-builtin/README.md)
- User-facing copy (the single source for any string a user can read: settings,
  manual, tool and extension descriptions, messages): [COPY.md](COPY.md).
  Whenever a task touches user-facing copy, in any package, follow it.
- Settings system architecture (schema, routing, persistence):
  [packages/tomat-shared/src/domain/settings/README.md](packages/tomat-shared/src/domain/settings/README.md)
- External services we depend on (HuggingFace, GitHub releases, R2) and how to
  fix breakage when they change: [EXTERNAL.md](EXTERNAL.md)

Tests are co-located with source as `*.test.ts`; scratch tests are
`*.tmp.test.ts` (gitignored anywhere). After a change, run `deno task check`,
`deno task fmt`, `deno task lint`, and `deno task test`.

Gate output is intentionally concise: a passing gate is roughly a dozen lines
(one `ok <package> <verb> (Ns)` line per package plus summaries), and each
failing package adds a bounded block with the last 20 output lines (which name
the failing test/file), a `rerun:` command, and a `full:` log path. Reading the
last ~50 lines of a gate run is sufficient even on failure. The complete
untruncated output of every package run is always written to
`.gate-logs/<verb>-<pkg>.log` (gitignored); read that file only when the
excerpt is not enough.

## General Rules

### 1. Think Before Coding

Don't assume. Don't hide confusion. Surface tradeoffs.

- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them. Don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

### 2. Simplicity First

Minimum code that solves the problem. Nothing speculative.

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.

### 3. Surgical Changes

Touch only what you must. Clean up only your own mess.

- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style.
- If you notice unrelated dead code, mention it. Don't delete it.

### 4. Goal-Driven Execution

Define success criteria. Loop until verified. For multi-step tasks, state a
brief plan with a verification check per step.
