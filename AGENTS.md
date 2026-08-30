# AGENTS.md

_Keep this file at altitude: a high-level overview, common conventions, and
important decisions only. Specifics (build/test commands, the persistence tree,
release mechanics, extension details) belong in the docs linked under
"Pointers", not here. When tempted to add something, prefer the canonical doc
and link it._

## Project Overview

Loca is a local-first modular AI client split into a long-running service and a
thin desktop UI that talk over an HTTP+WS API, plus one helper binary, a bundled
extension, and a distribution website. Tools come from two kinds of provider: an
**extension** (a `loca.json` bundle the core installs) and an **MCP server** (a
Model Context Protocol process the core connects to). The packages:

- **`loca-core-rs`**: the Rust core. One library that runs as the `loca-core`
  service binary on desktop and links into the Tauri app on mobile (via
  **`loca-core-ffi`**), so the phone embeds the real Core. It owns everything:
  the HTTPS/WSS transport, TLS + multi-client pairing auth, sessions and chat,
  LLM streaming (llama.cpp compiled in behind `local-inference`, GPU backends
  loaded as separate libraries), settings, memories + embedding relevance, local
  speech (sherpa-onnx compiled in behind `local-speech`) and external STT/TTS,
  model + binary downloads, sandboxed extension tool execution (a TypeScript
  worker under the pinned `deno`, in `workers/`), MCP server connections, the OS
  keychain, and self-update. Wire types under `src/contract/` are generated.
- **`@loca/core-updater`**: standalone Rust binary (`loca-core-updater`) that
  swaps in a staged core build during self-update, then restarts core. Separate
  because a process cannot replace the file it runs from on Windows.
- **`@loca/client`**: Tauri + Svelte desktop and mobile UI, intentionally
  "dumb": it renders, captures input, plays audio, manages global shortcuts, and
  talks to one or more paired cores.
- **`@loca/shared`**: TypeScript types + Zod validators (API contract, domain
  shapes, `loca.json` schema, WS frame unions) that the client, the website, and
  the contract generator consume, plus the shared UI layer under `./ui/*`
  (design tokens, UnoCSS preset, and the presentational Svelte components the
  client and website both render). The non-UI exports stay side-effect-free.
- **`@loca/model-catalog`**: hand-authored source for the signed model catalog.
- **`@loca/loca-extension-builtin`**: reference/baseline extension installed by
  default on a fresh core; doubles as the worked example for the `loca.json`
  format.
- **`@loca/loca-extension-samples`**: dev-only capability showcase exercising
  the full `loca.json` surface; NOT installed in production, only
  codebase-installed in the dev environment.
- **`@loca/website`**: Astro static site at `loca.rzkyif.com`: the landing page,
  the feature showcase, and the user manual. Renders the shared
  `@loca/shared/ui` components so demos match the app, and is multi-page (link
  navigation + view transitions, not an SPA) so navigation works without JS.

**Tech stack:** Rust (axum, tokio, rusqlite, llama.cpp, sherpa-onnx) for the
core; Tauri 2, Svelte 5, Vite, and UnoCSS for the client; Deno 2 for every
script, test runner, and TypeScript package. Deno-first workspace: no
`package.json`, no bun, no pnpm; the client invokes Vite and the Tauri CLI
through `npm:` specifiers from `deno task`.

**Key locations:**

- Domain types, the API contract, and the `loca.json` schema live in
  `packages/loca-shared/src/`. When changing the wire format, update this
  package first, then run `deno task contract` to regenerate
  `packages/loca-core-rs/src/contract/`; `deno task check` fails on stale
  output. See [scripts/contract/README.md](scripts/contract/README.md).
- HTTP routes: each core module owns its slice in
  `packages/loca-core-rs/src/<module>/routes.rs`, mounted by
  `src/transport/router.rs`. WS frames: `packages/loca-shared/src/api/ws.ts`.
  Boot wiring: `src/boot.rs`.
- Client API access: `packages/loca-client/src/ui/lib/core/`. Platform-specific
  Tauri calls: `packages/loca-client/src/ui/lib/platform/`.
- Extension author API: the Zod schema in
  `packages/loca-shared/src/validation/loca-json.ts` and the published JSON
  Schema in `packages/loca-shared/src/loca-json-schema.json`. The worker the
  core spawns for tools: `packages/loca-core-rs/workers/tool-worker.ts`.
- MCP server connections: `packages/loca-core-rs/src/mcp/`.

## Conventions

- **Tauri boundary (absolute rule):** nothing under
  `packages/loca-client/src/ui/` outside `lib/platform/` may import from
  `@tauri-apps/*` (`tauri.ts` desktop, `mobile.ts` android, their `shared.ts`
  helpers, and the `select.ts` OS-based bootstrap). Add a method to the
  `Platform` interface in `lib/platform/index.ts`, implement it in `tauri.ts`
  and `mobile.ts`, cover it in the `src/ui/test/platform-stub.ts` fixture, then
  call `platform().<namespace>.<method>()`. There is no web client. Enforced by
  the `loca/no-tauri-import` oxlint rule (`.ts`) and a `.svelte` grep pass, both
  in `deno task lint`.
- **Single-source UI (absolute rule):** every UI component the website renders
  must be the EXACT SAME shared `@loca/shared/ui` component the client renders,
  at EVERY layer (primitive AND composition: a field control, a field row, a
  section, a group header, a sidebar, a whole panel). "Shared" means BOTH sides
  render it: the client wraps the shared component feeding live state; the
  website wraps the same component feeding scripted/default state. A shared
  component that ONLY the website uses is NOT allowed. The fix for any
  represented-component mismatch is to push that layer into `@loca/shared/ui`
  and make the client consume it, NEVER to hand-mirror the client's markup on
  the website. Client-only behavior on a shared component (validation, pickers,
  capture, live catalogs) is injected via props/callbacks/snippets. The
  mechanism is a four-tier component taxonomy (A0 primitive / A `*View` / B thin
  client wrapper / C client shell) recorded in
  `packages/loca-client/src/ui/components/.tiers.json`, sample bundles + a
  website gallery, and three lint walkers; the canonical reference is
  [shared UI README](packages/loca-shared/src/ui/README.md). See also
  [website AGENTS.md](packages/loca-website/AGENTS.md).
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
- **Brand is Loca.** The product is written **Loca**, capitalized like any
  proper noun, in prose, headings, and user-facing copy. The two halves are
  **Core** (the service) and **Client** (the desktop UI), capitalized when named
  in a sentence, whether on their own (preferred: "Core", "Client") or with the
  brand ("Loca Core", "Loca Client"). Identifiers keep their own casing and stay
  lowercase: hyphenated package, crate, and binary names (`loca-core`), the
  `@loca/*` import scope, the all-caps `LOCA_*` env vars, `loca.rzkyif.com` and
  the `com.rzkyif.loca` bundle ids derived from it, the `~/.loca` paths, the
  `loca.json` manifest filename, release artifact filenames (`loca.pkg`), CSS
  classes (`loca-scroll`), and the `loca::<scope>` log targets.
- **Logging.** Core modules log through `tracing` with a `loca::<scope>`
  target; every line passes through `scrub_secrets` before it reaches `core.log`
  or stderr.
- **Icons.** UI icons are UnoCSS `presetIcons` classes (`i-<collection>-<name>`,
  almost always `i-material-symbols-*`). A missing icon emits no CSS and renders
  as an invisible box with no build error, so every icon name is validated
  against the bundled `@iconify/json` by the `check-icon-classes.ts` walker in
  `deno task lint`. Verify a name exists (e.g. at https://icones.js.org) before
  using it; not every icon has a `-rounded`/`-outline` variant.
- **Library reuse.** Prefer maintained crates and npm/JSR packages over
  hand-rolled code.
- **User-facing copy.** Every string a user can read (settings, manual, tool and
  extension descriptions, messages) follows the one guide at [COPY.md](COPY.md):
  voice, the canonical terminology glossary, and the per-surface rules. Read it
  before writing or editing any user-facing copy, in any package.

## Key Decisions

- **No non-consented network (absolute rule).** Once installed, the running Core
  and Client make NO outbound network request without an explicit user action:
  update checks happen on a button press, downloads behind a confirmation modal
  (see `downloads/` + the requirements routes + the client's
  `requestRequiredModal`). Boot and background code must not fetch, poll, check
  for updates, or download. The built-in extension is installed on first boot
  OFFLINE from artifacts fetched + verified during install (the installer
  payload carries `extensions/.loca-extension-builtin.{tgz,json}` when the
  build could stage it, and the Core binary's `bootstrap`/`install-service`
  subcommand, which every native installer's install hook wraps, plants them
  otherwise; boot seeding re-verifies the Ed25519 signature + tarball sha256 and
  installs them with zero network). If the plant is missing (an offline
  installer run), boot leaves the built-in for the user to install from the
  extensions UI rather than fetching. The ONLY place network is expected without
  a user action is the install phase itself, which runs before the app is
  considered installed. Build-time fetches on a developer machine (vendoring
  llama.cpp, sherpa-onnx prebuilts) are not covered by this rule.
- **Channels and persistence.** All state lives under `~/.loca/<channel>/`,
  where the channel (`stable` default, `dev`, `latest`) is selected by
  `LOCA_CHANNEL` and resolved identically in core, client, and the native
  installers. The one exception is `models/`, shared across channels at
  `~/.loca/models` so multi-GB weights are not re-downloaded per channel.
  Keychain entries, default ports, and service labels are channel-namespaced so
  two channels can run at once. See [DEVELOPMENT.md](DEVELOPMENT.md).
- **Trust root.** Release manifests are Ed25519-signed. The private key lives in
  a gitignored `.env`; the public key is committed in
  `packages/loca-core-rs/data/signing-keys.json` and baked into every compiled
  core by `build.rs`. Three signing identities are in play: (1) the Ed25519
  keypair signs `core.json`, `binaries.json`, and `android.json` (the Android
  client also verifies `android.json.sig` against the committed public key
  before self-updating); (2) a separate Tauri-format keypair signs `client.json`
  and the bundled desktop installers; (3) a Java keystore signs the Android APK
  for install. The keystore is supplied at release time via base64 in the
  gitignored `.env` (`LOCA_ANDROID_KEYSTORE_B64`); the decoded `*.jks` +
  `keystore.properties` are gitignored and never committed. A fourth, OPTIONAL
  identity pair applies to macOS (via the `APPLE_*` vars in `.env`): a Developer
  ID Application certificate signs + notarizes the macOS client bundle, and a
  Developer ID Installer certificate signs + notarizes the macOS `.pkg`
  installer. Both are inert when unset (ad-hoc signing, unsigned `.pkg`). The
  same `APPLE_*` env also gates + signs the iOS App Store build. A fifth,
  also-dormant identity is Windows Authenticode
  (`WINDOWS_CERTIFICATE_THUMBPRINT` / `WINDOWS_SIGN_COMMAND`). See
  [scripts/release/macos-signing.md](scripts/release/macos-signing.md).
- **Distribution split.** Two Cloudflare-hosted hostnames, aligned with content
  type: `loca.rzkyif.com` (Astro Worker, landing page only) and
  `get.loca.rzkyif.com` (R2 public bucket, every release artifact: installers,
  schemas, manifests, binaries). Both are defined in
  `packages/loca-core-rs/src/config.rs`.
- **Native features are opt-in at build time.** `local-inference` and
  `local-speech` compile vendored C/C++ trees and are OFF by default so a fresh
  checkout builds without `vendor/`; releases and `deno task dev` turn both on.
- **GPU backends load dynamically.** The core ships with the CPU baseline;
  per-GPU llama.cpp backends are separate shared libraries downloaded on
  consent. Mobile is CPU-only and downloads no executable code.
- **Byte-exact parity surfaces.** The TLS SPKI pin derivation, CPace, and the
  on-disk persistence tree must not change shape: already-paired clients and
  existing installs depend on them. Committed fixtures under
  `packages/loca-core-rs/src/*/testdata/` pin them.

## Pointers

For anything beyond the above, the canonical docs are:

- Build / run / test commands, channels: [DEVELOPMENT.md](DEVELOPMENT.md)
- Package vs release-item separation and the standardized per-package task
  vocabulary (`<verb>` / `<verb>:<pkg>`, fanned out by `scripts/pkg.ts`):
  [DEVELOPMENT.md](DEVELOPMENT.md). A package is a development unit (the root
  `deno.json` `workspace` array is the source of truth); a release item is a
  distribution unit that may span packages (`scripts/release/*.ts`, each
  declaring its `packages`).
- Per-package overview (layout, run/build/test, internals): the
  `packages/<name>/README.md` of the package in question, and
  `packages/loca-client/src/ui/lib/core/README.md` for the client transport.
- Test-suite guide (layout, helpers, fixtures, scratch tests, CI):
  [tests/README.md](tests/README.md). Two opt-in, local-only, never-in-CI E2E
  lanes sit alongside the co-located unit/component suites: a headless
  integration lane (real app in Chromium <-> real core over TLS, outbound deps
  mocked; the primary lane, happy paths only) and a tauri-driver smoke lane
  (native WebView + Rust transport). Deep dive:
  [tests/e2e/headless/README.md](tests/e2e/headless/README.md).
- Release + deploy, channels, Cloudflare + R2 setup:
  [packages/loca-website/README.md](packages/loca-website/README.md)
- macOS Developer ID signing + notarization:
  [scripts/release/macos-signing.md](scripts/release/macos-signing.md)
- Extension author API:
  [packages/loca-extension-builtin/README.md](packages/loca-extension-builtin/README.md)
- User-facing copy: [COPY.md](COPY.md).
- Settings system architecture (schema, routing, persistence):
  [packages/loca-shared/src/domain/settings/README.md](packages/loca-shared/src/domain/settings/README.md)
- External services we depend on (HuggingFace, GitHub releases, R2) and how to
  fix breakage when they change: [EXTERNAL.md](EXTERNAL.md)

Tests are co-located with source (`*.test.ts`, Rust `tests.rs` / inline
`#[cfg(test)]`); scratch tests are `*.tmp.test.ts` / `*.tmp.test.rs`
(gitignored anywhere). After a change, run `deno task check`, `deno task fmt`,
`deno task lint`, and `deno task test`.

Gate output is intentionally concise: a passing gate is roughly a dozen lines
(one `ok <package> <verb> (Ns)` line per package plus summaries), and each
failing package adds a bounded block with the last 20 output lines (which name
the failing test/file), a `rerun:` command, and a `full:` log path. The complete
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

### 5. Concise Code

Keep code concise and line counts low. Comments are short, to the point, and
added only when necessary: explain the non-obvious, never restate the code.
