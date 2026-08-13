# @tomat/client

The Tomat desktop UI: Tauri 2 + Svelte 5 + Vite + UnoCSS. It is intentionally
thin ("dumb"): it renders, captures input, plays audio, manages global
shortcuts, and talks to one or more paired cores over HTTP+WS. State and
computation belong in core; nothing here should grow its own persistence or
business logic.

## Layout

- `src/ui/`: the SvelteKit app. The Svelte/runes side lives at the top of
  `src/ui/`; `lib/` is reserved for pure-TS domain libraries.
  - `routes/`: the single-page route (`+page.svelte`, `+layout.svelte`).
  - `components/`: Svelte components, grouped by area (`chat/`, `new-core/`,
    `quick-settings/`, `session-list/`, `settings/`, `ui/`).
  - `composables/`: reusable Svelte-rune helpers (`use-*.svelte.ts`).
  - `state/`: app-wide reactive rune stores (`*.svelte.ts`).
  - `lib/core/`: the core API layer: typed HTTP/WS clients, one module per API
    area, plus paired-core management. Barrel export in `index.ts`.
  - `lib/platform/`: the platform abstraction (`index.ts` interface, `tauri.ts`
    desktop impl). See below.
  - `lib/`: other pure-TS domain libraries (appearance, capture, chat, settings,
    storage, tts/stt, util, window, ...).
  - `test/`: vitest support (`setup.ts`, the `platform-stub.ts` fixture).
- `src/tauri/`: the Rust shell: Tauri commands under `src/commands/`,
  window/channel/state plumbing, `capabilities/`, and `tauri.conf.json`.

## The Tauri boundary

Absolute rule: nothing under `src/ui/` outside `lib/platform/tauri.ts` may
import from `@tauri-apps/*`. To add a platform capability: add a method to the
`Platform` interface in `lib/platform/index.ts`, implement it in `tauri.ts` (and
a future `mobile.ts` when the mobile build lands), cover it in the
`src/ui/test/platform-stub.ts` fixture, then call
`platform().<namespace>.<method>()`. There is no web client, so there is no
browser implementation. Enforced by the `tomat/no-tauri-import` oxlint rule for
`.ts` files and a `.svelte` grep pass, both part of `deno task lint`.

## Run, build, test

All commands run from the repo root:

- `deno task dev`: run core and client together; see
  [DEVELOPMENT.md](../../DEVELOPMENT.md) for connecting the client to the dev
  core.
- `deno task build:client` / `deno task build:client:stable`: build the desktop
  bundle for the given channel.
- `deno task dev:client`: run just the client's Tauri dev loop in isolation.
- `deno task test:client`: vitest suite for the Svelte UI plus the Tauri crate's
  `cargo test`.
- `deno task check:client` / `deno task lint:client` / `deno task fmt:client`:
  the client's svelte-check, Tauri-crate clippy, and cargo fmt.
- `deno task test:e2e`: end-to-end tests; see
  [tests/e2e/tauri-driver/README.md](../../tests/e2e/tauri-driver/README.md).

## Further reading

- [`src/ui/lib/core/README.md`](src/ui/lib/core/README.md): the core API layer.
