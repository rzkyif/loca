# Loca test suite

Tests live co-located with source as `*.test.ts`. E2E specs are opt-in and live
under `tests/e2e/` (two lanes: a headless integration lane and a tauri-driver
smoke lane), each with its own runner.

## Filename convention

| Pattern              | Where                     | Gitignored? |
| -------------------- | ------------------------- | ----------- |
| `*.test.ts`          | next to source            | no          |
| `*.test.ts`          | `tests/e2e/<lane>/specs/` | no          |
| `*.tmp.test.{ts,rs}` | anywhere                  | **yes**     |

Rust uses inline `#[cfg(test)] mod tests` for unit tests. If integration tests
grow, they go in `packages/<pkg>/tests/*.rs` (Cargo's per-file convention).
Scratch Rust tests follow `*.tmp.test.rs` to inherit the gitignore.

Scratch tests are gitignored via `**/*.tmp.test.{ts,rs}` in the repo root
`.gitignore`. They run locally under the normal task but never reach commits or
CI.

## Quick reference

| Task                          | What it runs                                                                                                                                                                |
| ----------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `deno task test`              | All tests across every package (Deno test + vitest + cargo test).                                                                                                           |
| `deno task test:core-rs`      | Just `loca-core-rs` (the worker `.ts` tests + `cargo test`).                                                                                                                |
| `deno task test:shared`       | Just `loca-shared`.                                                                                                                                                         |
| `deno task test:client`       | Vitest against the Svelte 5 UI + the Tauri crate's `cargo test` (`packages/loca-client`).                                                                                   |
| `deno task test:core-updater` | `cargo test` for the updater crate.                                                                                                                                         |
| `deno task test:e2e:headless` | Headless integration E2E (real app in Chromium <-> real core over TLS, deps mocked). Manual only, primary lane. See [tests/e2e/headless/README.md](e2e/headless/README.md). |
| `deno task test:e2e`          | tauri-driver smoke (real Tauri shell via WebdriverIO). Manual only, thin native lane. See [tests/e2e/tauri-driver/README.md](e2e/tauri-driver/README.md).                   |

The aggregate `test`/`test:js` tasks print one `ok` line per package; on
failure they print the last 20 output lines plus a rerun command, and the
complete runner output is always in `.gate-logs/<verb>-<pkg>.log` (gitignored).
The runners use dot reporters (`deno test --reporter=dot`, `vitest --reporter=dot`,
`cargo test -q`), so per-test noise stays out of the console while failure
detail is still printed in full. See "Gate output" in
[DEVELOPMENT.md](../DEVELOPMENT.md).

## Layout

```
packages/<pkg>/src/foo.test.ts           # co-located test
packages/<pkg>/src/foo.tmp.test.ts       # scratch test (gitignored)
packages/loca-core-rs/src/<module>/tests.rs   # Rust module suites
packages/loca-core-rs/src/*/testdata/         # committed parity fixtures
tests/e2e/
  README.md                              # lane index: when to use which
  tauri-driver/                          # tauri-driver smoke lane (opt-in npm project)
    wdio.conf.ts                         # WebdriverIO config
    specs/*.test.ts                      # native smoke specs
  headless/                              # headless integration lane (opt-in npm project)
    harness/                             # the library (core-process, mock-services, app, pages)
    fixtures/                            # hermetic test extension + stdio MCP server
    specs/{tier1,tier2}/*.test.ts        # high-level happy-path specs
    specs/**/*.tmp.test.ts               # scratch specs (gitignored)
```

## E2E lanes (both opt-in, never in CI)

Reach for an E2E lane only when a change spans the client/core wire or the full
app boot; the co-located unit + component suites are the everyday tools.

- **Headless integration** (`test:e2e:headless`, the primary lane) mounts the
  real Svelte app in a real Chromium and drives it over real HTTP+WS+TLS against
  a real spawned `loca-core`, with every outbound dependency (LLM/STT/TTS,
  model + binary downloads) mocked locally. It is fast, deterministic, and
  cross-platform, and is where happy-path coverage lives. The harness is a
  library so specs read at a high level (`launchApp(...)` + page objects).
  Architecture, the behaviour delta versus tauri-driver, setup, and how to write
  a spec are in [e2e/headless/README.md](e2e/headless/README.md).
- **tauri-driver smoke** (`test:e2e`) drives the real Tauri shell to cover what
  headless deliberately cannot: the native WebView, the Rust `net` transport
  (rustls SPKI pinning), and OS-native calls. See
  [e2e/tauri-driver/README.md](e2e/tauri-driver/README.md).

**Happy paths, plus wire-only negatives in E2E.** Default to happy paths. The one
exception is a negative that only emerges from the real client<->core wire and
cannot be faithfully unit-tested (a wrong-code PAKE rejection, a provider error
mid-stream, a dropped-and-recovered connection). Any sad path with a faithful
seam belongs in a co-located unit/component test, not here.

## Writing tests against the core

The core is Rust (`packages/loca-core-rs`). Each module keeps its suite in a
sibling `tests.rs` or an inline `#[cfg(test)] mod tests`, and the shared
`testing.rs` modules (`downloads`, `update`, `llm`, `transport`, `secrets`) hold
the fixtures and fakes that more than one suite needs. External HTTP (LLM
providers, HuggingFace, npm, signed manifests) is mocked at the client seam the
module already declares; nothing in the suite reaches the network. Time-sensitive
logic takes a clock or a deadline argument rather than reading the wall clock.

The tool worker is TypeScript (`packages/loca-core-rs/workers/`) and keeps a
Deno test beside it that spawns the real worker and drives the NDJSON protocol.

## Writing Svelte component tests

Vitest + jsdom + `@testing-library/svelte`. Example:

```ts
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import Toggle from "./Toggle.svelte";

describe("Toggle", () => {
  it("calls onchange when toggled", async () => {
    const onchange = vi.fn();
    const { container } = render(Toggle, {
      props: { checked: false, onchange },
    });
    await fireEvent.click(container.querySelector("input")!);
    expect(onchange).toHaveBeenCalledWith(true);
  });
});
```

Gotchas:

- `$effect` runes don't run synchronously under jsdom. Drive them with
  `flushSync()` from `svelte` if your assertion depends on an effect.
- Mock the platform seam by stubbing `lib/platform/index.ts`. Don't bring Tauri
  into the test environment.
- The global test setup (`src/ui/test/setup.ts`) stubs `IntersectionObserver`,
  `ResizeObserver`, and `matchMedia` because jsdom doesn't ship them. If a test
  needs real DOM observer behavior, switch the environment to `happy-dom` or
  `vitest-browser-svelte`, but commit to one across the package.

## Writing Rust tests

Inline `#[cfg(test)] mod tests` for unit tests. The `KeychainStore` trait in
`packages/loca-client/src/tauri/src/commands/keychain.rs` is the pattern for
any test that would otherwise touch a platform API: define a small trait, ship a
`Real*` impl that wraps the real crate, and ship an `InMemory*` test impl. The
same pattern works for filesystem (write to a tempdir under `env::temp_dir()`),
system clock, and any OS resource that doesn't have a Rust-side mock.

Annotate the test module with `#[allow(clippy::unwrap_used)]` because the
crate's clippy config rejects `unwrap` in non-test code.

## CI

`.github/workflows/ci.yml` runs the always-on suite:

- The `deno` job runs the TypeScript pipeline on Linux: type-check, format
  check, JS/Svelte lint, and the Deno + vitest tests (`deno task test:js`).
- The `rs` matrix runs the Rust pipeline (clippy + `cargo test`) for the crates
  (tauri shell, core-rs, core-updater) on Linux, macOS, and Windows.
- Neither E2E lane (headless integration, tauri-driver smoke) runs in CI; both
  are opt-in and local-only.
