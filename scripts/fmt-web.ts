// Formats ONLY the file types oxfmt does not cover (.svelte, .astro, .css) with
// Prettier plus the same plugins the Svelte and Astro VS Code extensions bundle,
// so editor-on-save output and `deno task fmt` agree. Wired into the root
// `fmt` / `fmt:check` tasks next to oxfmt (.ts/.js/.json/.md) and the per-package
// (cargo) formatters; the glob below keeps it off every other formatter's turf.
//
// Versions track what the extensions ship: svelte-vscode -> prettier +
// prettier-plugin-svelte, astro-vscode -> prettier + prettier-plugin-astro.
// Options live in /.prettierrc.json (read below via resolveConfig and discovered
// by the extensions), aligned to oxfmt's defaults so the repo shares one style.
import { exists } from "@std/fs";
import { fromFileUrl } from "@std/path";
import * as prettier from "npm:prettier@3.9.1";
import * as sveltePlugin from "npm:prettier-plugin-svelte@4.1.1";
import * as astroPlugin from "npm:prettier-plugin-astro@0.14.1";

// Repo root as an OS path. `new URL("..").pathname` yields "/C:/work/" on Windows
// (an invalid cwd); fromFileUrl produces the correct native path on every OS.
const ROOT = fromFileUrl(new URL("..", import.meta.url));
const check = Deno.args.includes("--check");

// Non-ignored files (tracked + untracked), mirroring oxfmt's .gitignore handling.
const list = new Deno.Command("git", {
  args: ["ls-files", "--cached", "--others", "--exclude-standard", "*.svelte", "*.astro", "*.css"],
  cwd: ROOT,
  stdout: "piped",
});
const { stdout } = await list.output();
const files = new TextDecoder().decode(stdout).split("\n").filter(Boolean);

const plugins = [sveltePlugin, astroPlugin];
const unformatted: string[] = [];
for (const rel of files) {
  const path = `${ROOT}${rel}`;
  // --cached can list a file deleted in the working tree; skip those.
  if (!(await exists(path))) continue;
  const source = await Deno.readTextFile(path);
  const config = await prettier.resolveConfig(path);
  const formatted = await prettier.format(source, { ...config, filepath: rel, plugins });
  if (formatted === source) continue;
  unformatted.push(rel);
  if (!check) await Deno.writeTextFile(path, formatted);
}

if (check && unformatted.length > 0) {
  for (const rel of unformatted) console.log(rel);
  console.error(`!! ${unformatted.length} file(s) need formatting (run \`deno task fmt\`)`);
  Deno.exit(1);
}
console.log(
  `web fmt: ${files.length} checked, ${unformatted.length} ${check ? "unformatted" : "formatted"}.`,
);
