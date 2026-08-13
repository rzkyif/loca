// Tauri doesn't have a Node.js server to do proper SSR
// so we use adapter-static with a fallback to index.html to put the site in SPA mode
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/ for more info
//
// These two are fully-qualified `npm:` specifiers (versions kept in sync with
// deno.json) rather than the bare import-map aliases used elsewhere. svelte-kit
// sync and svelte-check load this config through @sveltejs/load-config via an
// absolute `file://` URL; on Windows that URL carries a lowercase drive letter,
// and Deno's workspace import-map scope (canonicalized to an uppercase drive)
// does not match it, so the bare aliases fail to resolve. `npm:` specifiers need
// no import map and resolve under either drive-letter case.
import adapter from "npm:@sveltejs/adapter-static@^3.0.10";
import { vitePreprocess } from "npm:@sveltejs/vite-plugin-svelte@^7.1.2";

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      fallback: "index.html",
    }),
    files: {
      lib: "src/ui/lib",
      routes: "src/ui/routes",
      appTemplate: "src/ui/app.html",
    },
    alias: {
      "@loca/shared": "../loca-shared/src/index.ts",
      "@loca/shared/*": "../loca-shared/src/*",
      // Svelte / runes side lives at the top of src/ui (not under lib, which
      // is reserved for pure-TS domain libraries). `$state` is avoided as an
      // alias because it collides with the Svelte 5 `$state` rune, so the
      // state folder is aliased as `$stores`.
      $components: "src/ui/components",
      "$components/*": "src/ui/components/*",
      $composables: "src/ui/composables",
      "$composables/*": "src/ui/composables/*",
      $stores: "src/ui/state",
      "$stores/*": "src/ui/state/*",
    },
  },
};

export default config;
