// Companion to the `loca/no-em-dash` oxlint rule, covering the tracked files
// oxlint never parses (Markdown, Rust, toml, json, yml, css, shell). Rejects the
// em dash (U+2014) wherever it appears: code, comments, strings, or prose.
//
// Wired into `deno task lint`. Keep this file em-dash-free, or it flags itself.

import { fromFileUrl } from "@std/path";

const EM_DASH = String.fromCharCode(0x2014);

// Extensions oxlint already lints via the no-em-dash rule. Skipped here so a
// single occurrence is never reported twice; oxlint stays the authority for
// these, this pass owns everything else.
const OXLINT_EXTS = new Set([
  ".ts",
  ".tsx",
  ".mts",
  ".cts",
  ".js",
  ".jsx",
  ".mjs",
  ".cjs",
  ".svelte",
]);

// Native OS path (fromFileUrl); URL .pathname is an invalid "/C:/..." cwd on Windows.
const ROOT = fromFileUrl(new URL("../../", import.meta.url));

interface Violation {
  file: string;
  line: number;
  column: number;
  text: string;
}

function extOf(path: string): string {
  const slash = path.lastIndexOf("/");
  const name = slash === -1 ? path : path.slice(slash + 1);
  const dot = name.lastIndexOf(".");
  return dot <= 0 ? "" : name.slice(dot);
}

async function trackedFiles(): Promise<string[]> {
  // `git ls-files` lists only tracked files, so node_modules, build artifacts,
  // and anything gitignored are excluded for free.
  const out = await new Deno.Command("git", {
    args: ["ls-files", "-z"],
    cwd: ROOT,
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (!out.success) {
    console.error("check-em-dash: `git ls-files` failed.");
    console.error(new TextDecoder().decode(out.stderr));
    Deno.exit(2);
  }
  return new TextDecoder()
    .decode(out.stdout)
    .split("\0")
    .filter((p) => p.length > 0);
}

async function scan(): Promise<Violation[]> {
  const violations: Violation[] = [];
  const decoder = new TextDecoder();
  for (const file of await trackedFiles()) {
    if (OXLINT_EXTS.has(extOf(file))) continue;
    let bytes: Uint8Array;
    try {
      bytes = await Deno.readFile(ROOT + file);
    } catch {
      continue; // unreadable (e.g. a broken symlink); nothing to scan
    }
    // Skip binaries the way `git grep -I` does: a NUL byte means not text.
    if (bytes.includes(0)) continue;
    const lines = decoder.decode(bytes).split("\n");
    for (let i = 0; i < lines.length; i++) {
      let col = lines[i].indexOf(EM_DASH);
      while (col !== -1) {
        violations.push({
          file,
          line: i + 1,
          column: col + 1,
          text: lines[i].trim(),
        });
        col = lines[i].indexOf(EM_DASH, col + 1);
      }
    }
  }
  return violations;
}

const violations = await scan();
if (violations.length > 0) {
  console.error(
    `Em dash (U+2014) is not allowed (${violations.length} occurrence(s) outside oxlint's reach):`,
  );
  for (const v of violations) {
    console.error(`  ${v.file}:${v.line}:${v.column}  ${v.text}`);
  }
  console.error(
    "\nReword the surrounding text so each sentence reads naturally without an em dash.",
  );
  Deno.exit(1);
}
