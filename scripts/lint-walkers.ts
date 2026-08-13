// Runs the repo's custom lint walkers concurrently: the checks oxlint cannot
// express (tauri-import bans in .svelte, the em-dash ban, shared-UI purity, the
// view / primitive / component-tier coverage walkers, the website composer
// check, icon-class validation, and shellcheck over the shell installer
// templates). Each walker is an
// independent read-only `git ls-files` scan, so they run in parallel; output is
// buffered and flushed as one labeled block per walker. Wired into the root
// `lint:js` task after oxlint. Exits non-zero if any walker fails.
//
// (oxlint-plugin.ts is not listed here: it is loaded by oxlint via
// .oxlintrc.json, not run as a standalone walker.)
//
// Usage: deno run -A scripts/lint-walkers.ts

import { dirname, fromFileUrl, join } from "@std/path";

const ROOT = dirname(dirname(fromFileUrl(import.meta.url)));
const PLUGINS = join(ROOT, "scripts", "lint-plugins");

const walkers = [
  "check-tauri-imports-svelte.ts",
  "check-em-dash.ts",
  "check-builtin-palette-color.ts",
  "check-shared-ui-purity.ts",
  "check-view-coverage.ts",
  "check-primitive-coverage.ts",
  "check-component-tiers.ts",
  "check-website-composer.ts",
  "check-icon-classes.ts",
  "check-shell-scripts.ts",
];

const results = await Promise.all(
  walkers.map(async (file) => {
    const { code, stdout, stderr } = await new Deno.Command("deno", {
      args: ["run", "-A", "--quiet", join(PLUGINS, file)],
      cwd: ROOT,
      stdout: "piped",
      stderr: "piped",
    }).output();
    const decoder = new TextDecoder();
    const out = (decoder.decode(stdout) + decoder.decode(stderr)).trimEnd();
    return { file, code, out };
  }),
);

let failed = 0;
for (const r of results) {
  if (r.out) console.log(`\n=== ${r.file} ===\n${r.out}`);
  if (r.code !== 0) {
    failed++;
    console.error(`!! ${r.file} failed (exit ${r.code})`);
  }
}

if (failed > 0) {
  console.error(`\n${failed} of ${walkers.length} lint walker(s) failed.`);
  Deno.exit(1);
}
console.log(`\nlint walkers: ${walkers.length} passed.`);
