import { defineConfig } from "astro/config";
import svelte from "@astrojs/svelte";
import mdx from "@astrojs/mdx";
import UnoCSS from "@unocss/astro";
import { fileURLToPath } from "node:url";

// Absolute path to the shared package source, so both `@loca/shared` (barrel)
// and `@loca/shared/ui/...` subpaths resolve in Vite/Astro the same way the
// client's svelte.config alias maps them. Prefix replacement: an import of
// `@loca/shared/ui/components/Bubble.svelte` resolves under this dir.
const sharedSrc = fileURLToPath(new URL("../loca-shared/src", import.meta.url));

// The repo root. The shared design system (base.css, the extracted Svelte
// components) and the hoisted node_modules both live above this package, so the
// dev server must be allowed to serve files from there, not just the website
// dir. Without this Vite blocks @astrojs/svelte's client runtime and the shared
// base.css as "outside the serving allow list".
const repoRoot = fileURLToPath(new URL("../..", import.meta.url));

// Pure-static, multi-page site (no SPA): link navigation works without JS and
// Astro's <ClientRouter /> layers view transitions on top when JS is present.
// Wrangler's static-assets feature (wrangler.toml) serves `./dist/` directly.
export default defineConfig({
  site: "https://loca.rzkyif.com",
  output: "static",
  trailingSlash: "ignore",
  // Keep Astro 6's HTML-aware whitespace compression. Astro 7 changed the default
  // to 'jsx', which collapses whitespace between adjacent inline elements (the way
  // React does) and can drop visible spaces in rendered copy. Pinning `true` keeps
  // the rendered output identical across the upgrade.
  compressHTML: true,
  build: {
    assets: "_astro",
    // Emit every stylesheet as an external file rather than inlining per-page
    // <style> blocks. Page-specific inlined styles (e.g. the home showcase's
    // UnoCSS icon utilities) are not reliably restored by <ClientRouter /> on
    // return navigation, so icons would vanish after manual -> home. External
    // stylesheets are persisted and deduped by href across view transitions.
    inlineStylesheets: "never",
  },
  integrations: [svelte(), mdx(), UnoCSS({ injectReset: "@unocss/reset/tailwind-v4.css" })],
  vite: {
    resolve: {
      alias: [
        { find: /^@loca\/shared$/, replacement: `${sharedSrc}/index.ts` },
        { find: /^@loca\/shared\//, replacement: `${sharedSrc}/` },
      ],
    },
    server: {
      fs: { allow: [repoRoot] },
    },
  },
});
