import { defineConfig } from "vitest/config";
import { svelte, vitePreprocess } from "@sveltejs/vite-plugin-svelte";
import { playwright } from "@vitest/browser-playwright";
import { fileURLToPath } from "node:url";
import { e2eCommands } from "./harness/commands.ts";

// Resolve workspace source dirs relative to this config.
const p = (rel: string) => fileURLToPath(new URL(rel, import.meta.url));
const CLIENT_UI = p("../../../packages/loca-client/src/ui");
const SHARED_SRC = p("../../../packages/loca-shared/src");

export default defineConfig({
  plugins: [svelte({ preprocess: vitePreprocess() })],
  server: { hmr: false },
  resolve: {
    conditions: ["browser", "import", "default"],
    alias: [
      // @client is the harness's handle on client UI internals.
      { find: "@client", replacement: CLIENT_UI },
      // Mirror the client's svelte.config / vitest aliases. ui subpath BEFORE
      // the bare-root exact match so "@loca/shared/ui/x" maps into the dir.
      { find: "@loca/shared/ui", replacement: `${SHARED_SRC}/ui` },
      { find: /^@loca\/shared$/, replacement: `${SHARED_SRC}/index.ts` },
      { find: "$lib", replacement: `${CLIENT_UI}/lib` },
      { find: "$components", replacement: `${CLIENT_UI}/components` },
      { find: "$composables", replacement: `${CLIENT_UI}/composables` },
      { find: "$stores", replacement: `${CLIENT_UI}/state` },
      { find: "$app/environment", replacement: `${CLIENT_UI}/test/app-environment.ts` },
      // @std/assert is a JSR import used by a few shared modules; shim it so the
      // npm/vite bundle resolves (browser code paths never assert in happy flow).
      { find: /^@std\/assert$/, replacement: p("./harness/std-assert-shim.ts") },
    ],
  },
  test: {
    include: ["specs/**/*.test.ts"],
    setupFiles: ["./setup.ts"],
    // Each spec spawns its own core(s); keep generous timeouts and run serially
    // so concurrent cores don't contend.
    testTimeout: 60_000,
    hookTimeout: 60_000,
    fileParallelism: false,
    // Per-file isolation: each spec file gets a fresh page + module graph, so the
    // client's module-singleton stores (sessions/downloads/connection/...) start
    // clean and never leak across files. Within a file, a spec must reset what it
    // dirties (afterEach -> app.dispose()) and read async state via vi.waitFor,
    // never assert a singleton synchronously right after an async trigger.
    isolate: true,
    browser: {
      enabled: true,
      provider: playwright(),
      headless: true,
      // Node-side actions callable from in-browser tests (spawn/stop core, etc.).
      commands: e2eCommands,
      // Chromium trusts the core's self-signed loopback TLS via ignoreHTTPSErrors;
      // fake media devices let the STT spec feed canned speech to getUserMedia.
      instances: [
        {
          browser: "chromium",
          context: { ignoreHTTPSErrors: true },
          launch: {
            args: [
              "--use-fake-ui-for-media-stream",
              "--use-fake-device-for-media-stream",
              "--autoplay-policy=no-user-gesture-required",
            ],
          },
        },
      ],
    },
  },
});
