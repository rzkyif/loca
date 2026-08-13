import { defineConfig } from "unocss";
// Relative path (not the @loca/shared alias): uno.config.ts is loaded by
// UnoCSS via jiti, outside Vite/Deno resolution, so the package alias is not
// available here; jiti resolves this file directly and its npm deps from
// node_modules.
import { locaUnoBase } from "../loca-shared/src/ui/uno-preset.ts";

// The presets, font theme, color/surface shortcuts, and roundedness rules are
// shared with the website via locaUnoBase() (packages/loca-shared/src/ui).
// Only the content pipeline (which files to scan for classes) is client-local;
// it scans this app's source plus the extracted shared components so their
// classes are generated here too.
export default defineConfig({
  ...locaUnoBase(),
  content: {
    pipeline: {
      include: [
        /\.(vue|svelte|[jt]sx?|mdx?|astro|elm|php|phtml|html)($|\?)/,
        /loca-shared\/src\/ui\/.*\.svelte($|\?)/,
      ],
    },
  },
});
