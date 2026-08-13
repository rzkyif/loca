import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import UnoCSS from "unocss/vite";
import { patchKitTdzRace, restartOnRustChange } from "./vite-dev-plugins";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async ({ command }) => ({
  // patchKitTdzRace + restartOnRustChange are dev-only workarounds for the
  // WebKit ESM TDZ race in SvelteKit's bootstrap; see vite-dev-plugins.ts.
  plugins: [patchKitTdzRace(), UnoCSS(), sveltekit(), !host && restartOnRustChange()].filter(
    Boolean,
  ),

  // Quiet the dev server: `deno task dev` multiplexes this output under the
  // "client" badge, where vite's HMR/reload/ready chatter drowns out the app's
  // own logging. "warn" drops that info noise but keeps transform/import/syntax
  // errors and warnings visible. Gated to the dev server ("serve") so production
  // `vite build` output is unchanged.
  logLevel: (command === "serve" ? "warn" : "info") as "warn" | "info",

  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // Don't HMR on Rust/Cargo output, and don't HMR the en-masse rewrites
      // svelte-kit sync makes to .svelte-kit/generated (the burst re-triggers
      // the WebKit TDZ race; see vite-dev-plugins.ts). Route HMR still works
      // via src/ui/routes/; only route add/delete needs a manual refresh.
      ignored: ["**/src/tauri/**", "**/.svelte-kit/generated/**"],
    },
  },

  // Pre-bundle deps that aren't in the static module graph (marked /
  // highlight.js / marked-highlight are dynamically imported in
  // MessageMarkdown.svelte's ensureRenderer()) so Vite doesn't discover them
  // mid-bootstrap and force a full reload that re-triggers the WebKit TDZ race
  // (see vite-dev-plugins.ts). Dev-only; the production lazy-load is unaffected.
  optimizeDeps: {
    include: [
      "dompurify",
      "@tauri-apps/api/core",
      "@tauri-apps/api/event",
      "@tauri-apps/plugin-opener",
      "marked",
      "highlight.js",
      "marked-highlight",
    ],
  },

  build: {
    // The SvelteKit page graph plus UnoCSS runtime and tauri-api surface
    // pushes the eager chunk past the 500 kB default. Bumped to 1000 kB so
    // we still get a fresh warning if something genuinely heavy lands.
    chunkSizeWarningLimit: 1000,
  },
}));
