#!/usr/bin/env -S deno run -A
// Starts the Astro dev server for the website. No staging. Release artifacts
// (manifests, installers, schemas) live on R2 at get.loca.rzkyif.com now,
// not on the Worker, so the landing page is fully self-contained.

import { dirname, fromFileUrl, join, resolve } from "@std/path";

const REPO_ROOT = resolve(dirname(fromFileUrl(import.meta.url)), "../..");
const WEBSITE_DIR = join(REPO_ROOT, "packages/loca-website");

// Force Astro's in-process foreground dev server. Astro 7's `astro dev` switches
// to a detached "background" dev server when it detects an agentic environment
// (via am-i-vibing), and that mode re-spawns `<root>/node_modules/astro/bin/astro.mjs`
// with the host runtime. Under Deno's hoisted node_modules there is no per-package
// astro bin at that path, so the child dies with "Module not found" and the parent
// only reports "Dev server process exited before becoming ready." Setting
// ASTRO_DEV_BACKGROUND=1 marks this process as the server itself, so Astro runs the
// dev server in-process (the Astro 6 behaviour) which works under Deno. The website's
// `dev` task in deno.json sets the same variable for `deno task dev:website`.
const cmd = new Deno.Command("deno", {
  args: ["run", "-A", "npm:astro@^7", "dev"],
  cwd: WEBSITE_DIR,
  env: { ASTRO_DEV_BACKGROUND: "1" },
  stdout: "inherit",
  stderr: "inherit",
});
Deno.exit((await cmd.output()).code);
