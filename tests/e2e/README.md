# End-to-end lanes

Two opt-in E2E lanes live here, each in its own subdirectory with its own
runner. Both are **manual only and never run in CI**; the co-located unit +
component suites are the everyday tools. Reach for a lane only when a change
spans the client/core wire or the full app boot.

| Lane                               | Directory                                 | Runs                          | Use it for                                                                                                                                                                                      |
| ---------------------------------- | ----------------------------------------- | ----------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **headless integration** (primary) | [`headless/`](headless/README.md)         | `deno task test:e2e:headless` | The full product behaviour of client <-> core: real Svelte app in real Chromium over real HTTP+WS+TLS to a real spawned `loca-core`, outbound deps mocked. Fast, deterministic, cross-platform. |
| **tauri-driver smoke** (thin)      | [`tauri-driver/`](tauri-driver/README.md) | `deno task test:e2e`          | Only what headless cannot: the native WebView engine, the Rust `net` transport (rustls SPKI pinning), and OS-native calls. Slow, platform-specific.                                             |

The two are complementary: headless owns the breadth of happy-path coverage; the
tauri-driver lane is a thin smoke test for the native seams headless stubs out.
The exact behaviour delta between them is documented in
[headless/README.md](headless/README.md).

## Why these are npm projects, not Deno

Both runners are Node-only browser-automation toolchains (WebdriverIO +
`tauri-driver`; vitest browser mode + Playwright/Chromium) that Deno can't host,
so each lane keeps its dependencies in a local npm install rather than the
workspace `deno.json`. This is deliberate: it keeps the heavy toolchains opt-in
and off developer machines that never run E2E. The whole `tests/e2e/` tree is
excluded from `deno fmt`/`lint`/`check`. The entry point still stays
Deno-native: you invoke each lane through a `deno task` wrapper
(`scripts/test-e2e*.ts`), which execs the lane's local runner.

## Setup and specs

Per-lane setup (the one-time toolchain install), the spec layout, and the
when-to-write guidance live in each lane's own README:

- [headless/README.md](headless/README.md) - architecture, the behaviour delta,
  setup, and how to write a spec.
- [tauri-driver/README.md](tauri-driver/README.md) - setup, platform notes, and
  what to reserve the native lane for.

Permanent specs are `*.test.ts`; agent scratch specs are `*.tmp.test.ts` and are
gitignored everywhere.
