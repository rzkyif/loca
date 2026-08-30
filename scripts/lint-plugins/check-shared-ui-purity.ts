// Purity check for the shared UI layer (Tier A0 primitives + Tier A `*View`s).
// A shared component must have no expectations from its host: it may not import
// client code (`@loca/client`, the `$lib`/`$stores`/`$composables` aliases),
// `@tauri-apps/*`, or the API/network layer (`@loca/shared/api/*`). Network and
// platform access is impossible transitively once client imports are banned, so
// a presentational View renders identically in the client, the website, and a
// future mobile build. See packages/loca-shared/src/ui/README.md.
//
// Companion to the oxlint `loca/no-tauri-import` family (oxlint can't parse
// .svelte). Wired into `deno task lint`.

import { walk } from "@std/fs/walk";
import { fromFileUrl } from "@std/path";

const STRICT = true;
// Native OS path (fromFileUrl); URL .pathname breaks walk()/readdir on Windows.
// Scans the WHOLE shared UI subtree, not just components/: its `.ts` modules are
// equally part of the single-source layer. oxlint covers `.ts` but only for
// @tauri-apps; this walker also bans the client aliases and the api layer, and is
// the sole check for `.svelte`.
const ROOT = fromFileUrl(new URL("../../packages/loca-shared/src/ui/", import.meta.url));
const REL = "packages/loca-shared/src/ui/";
// Forbidden import sources for a shared presentational component.
const FORBIDDEN =
  /(?:from\s+|import\s+)["'](?:@loca\/client|\$lib|\$stores|\$composables|@tauri-apps\/|@loca\/shared\/api)/;

interface Violation {
  file: string;
  lineNumber: number;
  line: string;
}

async function scan(): Promise<Violation[]> {
  const violations: Violation[] = [];
  for await (const entry of walk(ROOT, { exts: [".svelte", ".ts"], includeDirs: false })) {
    // Tests aren't part of the rendered single-source layer.
    if (entry.path.endsWith(".test.ts")) continue;
    const text = await Deno.readTextFile(entry.path);
    const lines = text.split("\n");
    for (let i = 0; i < lines.length; i++) {
      if (FORBIDDEN.test(lines[i])) {
        violations.push({
          file: REL + entry.path.slice(ROOT.length).replaceAll("\\", "/"),
          lineNumber: i + 1,
          line: lines[i].trim(),
        });
      }
    }
  }
  return violations;
}

const violations = await scan();
if (violations.length > 0) {
  const label = STRICT ? "ERROR" : "WARN";
  console.error(`[${label}] shared UI components must not import client/tauri/api code:`);
  for (const v of violations) {
    console.error(`  ${v.file}:${v.lineNumber}  ${v.line}`);
  }
  console.error(
    "\nInject client-only behavior via props/callbacks/snippets instead; keep the View presentational.",
  );
  if (STRICT) Deno.exit(1);
}
