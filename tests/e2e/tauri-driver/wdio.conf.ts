// WebdriverIO config for E2E. Drives the built debug Tauri binary
// through tauri-driver. Manual-only; see tests/e2e/tauri-driver/README.md for setup.
//
// One-time setup (do this before invoking `deno task test:e2e`):
//   1. cargo install tauri-driver --locked
//   2. deno task build:client   # produces a debug Tauri binary
//   3. Confirm `tauri-driver --version` works on PATH
//
// The official Tauri 2 docs (https://v2.tauri.app/develop/tests/webdriver/)
// describe the platform notes. As of darwin 25.4 (macOS 26) the support
// status hasn't been verified. This conf is a starting point, not a
// proven-working harness. If you hit issues, document them in
// tests/e2e/tauri-driver/README.md and fall back to manual verification with
// `deno task dev`.

import { type ChildProcess, spawn } from "node:child_process";
import process from "node:process";

let driver: ChildProcess | undefined;

const tauriBinary =
  process.env.LOCA_TAURI_BIN ?? "packages/loca-client/src/tauri/target/debug/loca";

export const config: WebdriverIO.Config = {
  hostname: "127.0.0.1",
  port: 4444,
  specs: [
    // Permanent specs (committed) + agent scratch specs (gitignored via
    // `**/*.tmp.test.ts` in the repo root .gitignore; both globs match).
    "./specs/**/*.test.ts",
  ],
  maxInstances: 1,
  capabilities: [
    {
      // tauri-driver translates this to the host's WebView driver.
      "tauri:options": { application: tauriBinary },
      browserName: "wry",
    } as WebdriverIO.Capabilities,
  ],
  reporters: ["spec"],
  framework: "mocha",
  mochaOpts: { ui: "bdd", timeout: 60_000 },
  // Start tauri-driver before the suite runs and clean it up after.
  // tauri-driver supervises the platform driver (msedgedriver on
  // Windows, safaridriver on macOS, WebKitWebDriver on Linux).
  onPrepare() {
    driver = spawn("tauri-driver", [], { stdio: "inherit" });
  },
  onComplete() {
    driver?.kill();
  },
};
