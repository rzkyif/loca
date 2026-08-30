// Companion to the `loca/no-builtin-palette-color` oxlint rule, covering the
// tracked files oxlint never parses (.astro, .css, Markdown, ...). A built-in
// UnoCSS/Tailwind palette color paints a fixed sRGB value that ignores the
// appearance settings and does not theme-invert; use the themable tokens
// (*-accent-{blue|purple|red|green|yellow}-N, *-default-N) instead.
//
// Wired into `deno task lint`. This source keeps no literal palette-color token,
// or it flags its own pattern.

import { fromFileUrl } from "@std/path";

// KEEP IN SYNC with PALETTE_COLOR_RE in oxlint-plugin.ts.
const PALETTE_COLOR_RE =
  /\b(?:text|bg|border|ring|from|to|via|fill|stroke|outline|divide|caret|decoration|shadow)-(?:slate|gray|zinc|neutral|stone|red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose)-(?:50|[1-9]00|950)\b/g;

// Extensions oxlint already lints via the no-builtin-palette-color rule. Skipped
// here so a single occurrence is never reported twice; oxlint stays the
// authority for these, this pass owns everything else.
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
    console.error("check-builtin-palette-color: `git ls-files` failed.");
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
      PALETTE_COLOR_RE.lastIndex = 0;
      let m = PALETTE_COLOR_RE.exec(lines[i]);
      while (m !== null) {
        violations.push({
          file,
          line: i + 1,
          column: m.index + 1,
          text: lines[i].trim(),
        });
        m = PALETTE_COLOR_RE.exec(lines[i]);
      }
    }
  }
  return violations;
}

const violations = await scan();
if (violations.length > 0) {
  console.error(
    `Built-in palette color utility is not allowed (${violations.length} occurrence(s) outside oxlint's reach):`,
  );
  for (const v of violations) {
    console.error(`  ${v.file}:${v.line}:${v.column}  ${v.text}`);
  }
  console.error(
    "\nUse a themable token: *-accent-{blue|purple|red|green|yellow}-N for accents " +
      "or *-default-N for neutrals, so the color follows the appearance settings " +
      "and dark-mode inversion.",
  );
  Deno.exit(1);
}
