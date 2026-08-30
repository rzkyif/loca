// Rejects UnoCSS icon classes naming an icon absent from the bundled
// `@iconify/json`. presetIcons emits NOTHING for a missing icon: no CSS rule, no
// build error, just an invisible element, so a typo like
// `i-material-symbols-chevron-rihgt-rounded` renders as a blank box.
//
// Resolution mirrors presetIcons: strip the `i-` prefix and any `?...` modifier,
// split into <collection>-<name> against the collections that exist in the
// bundle (longest prefix wins), then confirm the name is a real icon or alias. A
// token whose prefix is NOT a known collection is a custom `i-*` utility, not an
// icon, and is left alone.
//
// Wired into `deno task lint`. There is no companion oxlint rule: a plugin runs
// per-file and cannot load the Iconify collections, and this walker already
// covers every file type uniformly.

import { lookupCollection, lookupCollections } from "npm:@iconify/json@^2.2.483";
import { fromFileUrl, relative } from "@std/path";

// Native OS path (fromFileUrl); URL .pathname is an invalid "/C:/..." cwd on Windows.
const ROOT = fromFileUrl(new URL("../../", import.meta.url));

// This file's own comments document what a BAD icon class looks like (deliberate
// typos, e.g. `...chevron-rihgt...`), so scanning it would flag its own teaching
// examples. A lint script never renders an icon, so skip it: nothing real is
// lost, and the negative examples can stay readable inline.
// `relative` + forward-slash normalization so this matches the git-listed path
// (native backslashes on Windows) compared against `file` in the scan below.
const SELF = relative(ROOT, fromFileUrl(import.meta.url)).replaceAll("\\", "/");

// File types whose source can contain an icon class. Matches the union of the
// client and website UnoCSS `content` pipelines.
const SCAN_EXTS = new Set([".svelte", ".ts", ".tsx", ".jsx", ".mdx", ".astro", ".html"]);

// An `i-` utility token: `i-` followed by lowercase/digit segments joined by `-`
// or a single `:` (the `i-mdi:github` form). The leading boundary rejects
// matches inside identifiers (`api-foo`, `multi-line`) while still allowing a
// variant prefix (`hov:i-...`, `dark:i-...`). A trailing `?mode` modifier is
// captured separately so it can be stripped before resolution.
const TOKEN_RE = /(?<![\w-])i-[a-z0-9]+(?:[-:][a-z0-9]+)*(\?[a-z0-9]+)?/g;

interface Violation {
  file: string;
  line: number;
  column: number;
  token: string;
}

function extOf(path: string): string {
  const slash = path.lastIndexOf("/");
  const name = slash === -1 ? path : path.slice(slash + 1);
  const dot = name.lastIndexOf(".");
  return dot <= 0 ? "" : name.slice(dot);
}

async function trackedFiles(): Promise<string[]> {
  const out = await new Deno.Command("git", {
    args: ["ls-files", "-z"],
    cwd: ROOT,
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (!out.success) {
    console.error("check-icon-classes: `git ls-files` failed.");
    console.error(new TextDecoder().decode(out.stderr));
    Deno.exit(2);
  }
  return new TextDecoder()
    .decode(out.stdout)
    .split("\0")
    .filter((p) => p.length > 0);
}

// The set of collection prefixes present in the bundle (e.g. "material-symbols",
// "line-md", "mdi"). Light metadata only; the per-collection icon data is loaded
// lazily and cached as collections are encountered.
const collectionPrefixes = new Set(Object.keys(await lookupCollections()));
const iconCache = new Map<string, Set<string>>();

async function iconNames(prefix: string): Promise<Set<string>> {
  let names = iconCache.get(prefix);
  if (!names) {
    const data = await lookupCollection(prefix);
    names = new Set([...Object.keys(data.icons), ...Object.keys(data.aliases ?? {})]);
    iconCache.set(prefix, names);
  }
  return names;
}

// All (collection, name) splits of a bare icon body whose collection exists in
// the bundle. `material-symbols-light-foo` yields both the `material-symbols`
// and `material-symbols-light` splits; the token is valid if ANY of them
// resolves, which is exactly what presetIcons does when collection names overlap.
function candidateSplits(body: string): Array<{ prefix: string; name: string }> {
  // Explicit collection:name form is unambiguous.
  const colon = body.indexOf(":");
  if (colon !== -1) {
    const prefix = body.slice(0, colon);
    return collectionPrefixes.has(prefix) ? [{ prefix, name: body.slice(colon + 1) }] : [];
  }
  const splits: Array<{ prefix: string; name: string }> = [];
  for (let i = body.indexOf("-"); i !== -1; i = body.indexOf("-", i + 1)) {
    const prefix = body.slice(0, i);
    if (collectionPrefixes.has(prefix)) splits.push({ prefix, name: body.slice(i + 1) });
  }
  return splits;
}

async function isMissing(token: string): Promise<boolean> {
  // `i-mdi-foo?bg` -> body `mdi-foo` (the `?bg` mode never changes which icon).
  const body = token.slice(2).split("?")[0];
  const splits = candidateSplits(body);
  // No known collection prefix: this is a custom `i-*` utility, not an icon.
  if (splits.length === 0) return false;
  for (const { prefix, name } of splits) {
    if (name.length > 0 && (await iconNames(prefix)).has(name)) return false;
  }
  return true;
}

async function scan(): Promise<Violation[]> {
  const violations: Violation[] = [];
  const decoder = new TextDecoder();
  for (const file of await trackedFiles()) {
    if (file === SELF) continue;
    if (!SCAN_EXTS.has(extOf(file))) continue;
    let bytes: Uint8Array;
    try {
      bytes = await Deno.readFile(ROOT + file);
    } catch {
      continue;
    }
    if (bytes.includes(0)) continue;
    const lines = decoder.decode(bytes).split("\n");
    for (let i = 0; i < lines.length; i++) {
      for (const m of lines[i].matchAll(TOKEN_RE)) {
        if (await isMissing(m[0])) {
          violations.push({ file, line: i + 1, column: m.index + 1, token: m[0] });
        }
      }
    }
  }
  return violations;
}

const violations = await scan();
if (violations.length > 0) {
  console.error(
    `Unknown icon class (${violations.length}): the named icon is not in @iconify/json and renders as nothing:`,
  );
  for (const v of violations) {
    console.error(`  ${v.file}:${v.line}:${v.column}  ${v.token}`);
  }
  console.error(
    "\nCheck the exact name at https://icones.js.org (the collection prefix must match too).",
  );
  Deno.exit(1);
}
