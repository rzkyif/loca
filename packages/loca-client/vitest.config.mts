import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { svelteTesting } from "@testing-library/svelte/vite";

// Vitest config for the Svelte 5 UI. Kept separate from vite.config.ts
// because vitest only needs the Svelte plugin (no SvelteKit, no UnoCSS,
// no Tauri-restart watcher), and pulling in SvelteKit's plugin under
// vitest pulls in the $app/* virtual modules which we mock instead.
//
// Tests live next to their source files as `*.test.ts`. The `.tmp.test.ts`
// variant is gitignored agent-scratch tests; both globs are picked up.

export default defineConfig({
  // Bare svelte plugin. `hot: false` is not a valid plugin option, and
  // the plugin's configureServer hook tries to wire HMR even when vitest
  // doesn't want it; `server.hmr: false` below short-circuits that path.
  // autoCleanup is wired manually in src/ui/test/setup.ts because
  // svelteTesting()'s default cleanup setup file lives under
  // node_modules/.deno/... where Vite's loader can't resolve it.
  plugins: [svelte(), svelteTesting({ autoCleanup: false })],
  resolve: {
    alias: {
      // Subpath exports (`@loca/shared/ui/...`) must be aliased to the source
      // dir BEFORE the bare alias, which only maps the package root to index.ts.
      // Mirrors the `@loca/shared/*` mapping in svelte.config.js.
      "@loca/shared/ui": new URL("../loca-shared/src/ui", import.meta.url).pathname,
      "@loca/shared": new URL("../loca-shared/src/index.ts", import.meta.url).pathname,
      // SvelteKit's `$lib` alias is supplied here for vitest since we don't
      // load the SvelteKit plugin (see top-of-file comment).
      $lib: new URL("./src/ui/lib", import.meta.url).pathname,
      // The Svelte / runes side lives at the top of src/ui. These mirror the
      // custom aliases in svelte.config.js (kept in sync manually because
      // vitest does not load the SvelteKit plugin). `$stores` aliases the
      // `state/` folder to avoid colliding with the Svelte 5 `$state` rune.
      $components: new URL("./src/ui/components", import.meta.url).pathname,
      $composables: new URL("./src/ui/composables", import.meta.url).pathname,
      $stores: new URL("./src/ui/state", import.meta.url).pathname,
      // SvelteKit virtual modules. We don't run SvelteKit under vitest, so
      // these point at tiny shim files that export the same surface.
      "$app/environment": new URL("./src/ui/test/app-environment.ts", import.meta.url).pathname,
    },
  },
  server: { hmr: false },
  test: {
    environment: "jsdom",
    include: ["src/ui/**/*.test.ts"],
    setupFiles: ["./src/ui/test/setup.ts"],
    globals: false,
    // Run the suite serially per file. Svelte 5 runes share a hidden
    // global effect-tracking context, and parallel runs in jsdom can
    // produce flaky teardown order. Drop this once we have a real
    // motivating reason for parallelism.
    pool: "forks",
    fileParallelism: false,
  },
});
