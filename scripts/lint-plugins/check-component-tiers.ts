// Tier-manifest check. Every client component under src/ui/components/ must be
// classified exactly once in components/.tiers.json (tier A0/A/B/C), and every
// classified component must provably map to gallery coverage so "every client
// component is visible in the gallery" holds:
//   - Tier B names the single shared `*View` it wraps; that View must exist AND
//     be galleried (in GALLERY_VIEWS).
//   - Tier C must not carry its own un-extracted raw styled markup: the
//     raw-leaf cap is the hard gate, so every visible leaf is a galleried View /
//     primitive or a thin layout wrapper. A Tier C may optionally declare
//     `composes: [ViewNames]` and/or `orchestratorOf: [children]` to document how
//     it is covered; declarations are validated when present.
// See packages/loca-shared/src/ui/README.md.
//
// Wired into `deno task lint`. The base classification checks are always strict.
// The gallery-coverage contract (raw-markup cap / composes-when-declared /
// wraps-is-galleried) is gated behind STRICT_GALLERY.

import { walk } from "@std/fs/walk";
import { fromFileUrl } from "@std/path";
import {
  EMBEDDED_VIEWS,
  GALLERY_VIEWS,
} from "../../packages/loca-website/src/components/gallery/registry.ts";

const STRICT_GALLERY = true;
// A Tier-C orchestrator may keep a few raw layout wrappers, but past this many
// styled native leaves it is hiding un-extracted markup that belongs in a shared
// View. A regression heuristic, not a completeness proof.
const RAW_LEAF_CAP = 4;
// Native OS path (fromFileUrl); URL .pathname breaks walk()/readdir on Windows.
const ROOT = fromFileUrl(new URL("../../", import.meta.url));
const COMPONENTS = `${ROOT}packages/loca-client/src/ui/components/`;
const MANIFEST = `${COMPONENTS}.tiers.json`;
const SHARED_COMPONENTS = `${ROOT}packages/loca-shared/src/ui/components/`;

type Entry = {
  tier: "A0" | "A" | "B" | "C";
  wraps?: string;
  composes?: string[];
  orchestratorOf?: string[];
  unsharedLeaves?: string[];
};
const manifest: { components: Record<string, Entry> } = JSON.parse(
  await Deno.readTextFile(MANIFEST),
);

const onDisk = new Set<string>();
for await (const e of walk(COMPONENTS, { exts: [".svelte"], includeDirs: false })) {
  if (e.path.endsWith(".test.svelte")) continue;
  // Normalize to forward slashes so keys match .tiers.json (walk yields native
  // backslash paths on Windows).
  onDisk.add(e.path.slice(COMPONENTS.length).replaceAll("\\", "/"));
}

// Shared `*View` basenames, found recursively (Views live in domain subfolders),
// so a Tier-B `wraps` / Tier-C `composes` target resolves by name regardless of
// which subfolder it moved into.
const sharedViews = new Set<string>();
for await (const e of walk(SHARED_COMPONENTS, { exts: [".svelte"], includeDirs: false })) {
  sharedViews.add(e.name.replace(/\.svelte$/, ""));
}

// A View is "covered by the gallery" if it has its own card (GALLERY_VIEWS) or is
// shown transitively inside a parent's card (EMBEDDED_VIEWS). Either satisfies a
// Tier-B `wraps` / Tier-C `composes` target, so the client may wrap/compose an
// embedded View (e.g. DownloadRow wraps DownloadRowView, shown via DownloadsModal).
const galleryViews = new Set<string>([...GALLERY_VIEWS, ...Object.keys(EMBEDDED_VIEWS)]);

// A styled native leaf: a lowercase HTML element carrying a class with a visual
// (non-layout) utility. Layout-only utilities (flex/grid/gap/p-/m-/w-/h-/...) are
// deliberately not counted, so an arrangement wrapper does not trip the cap.
const VISUAL_UTILITY_RE =
  /\b(?:bg-|border(?:-|\b)|rounded|shadow|ring-|divide-|fill-|stroke-|from-|to-|via-|text-(?:xs|sm|base|lg|xl|[2-9]xl|default|accent|surface|inverse)|font-(?:medium|semibold|bold))/;
const NATIVE_TAG_WITH_CLASS_RE = /<([a-z][a-z0-9]*)\b[^>]*?\bclass\s*=\s*(["'{])([^"'}]*)/g;
// Raw native form controls belong in a shared primitive (Input/Select/Textarea),
// never hand-rolled in a client shell, so each one counts toward the cap
// regardless of its class.
const NATIVE_CONTROL_RE = /<(?:select|input|textarea)\b/g;
// Bespoke visual chrome expressed through inline styles rather than utility
// classes (the MessageStack edge-fade masks were exactly this): a `style="..."`
// or Svelte `style:` directive carrying mask / background / shadow / filter /
// clip-path. The class-based heuristic is blind to these, so count them too.
const INLINE_VISUAL_STYLE_RE =
  /style:(?:mask|background|box-shadow|filter|clip-path|-webkit-mask)\b|style\s*=\s*["'][^"']*(?:mask|background|box-shadow|filter|clip-path)/g;

function countMatches(re: RegExp, src: string): number {
  re.lastIndex = 0;
  let n = 0;
  while (re.exec(src) !== null) n++;
  return n;
}

function countStyledLeaves(svelteSrc: string): number {
  // Only the markup region: drop the <script> blocks and <style>.
  const markup = svelteSrc
    .replace(/<script[\s\S]*?<\/script>/g, "")
    .replace(/<style[\s\S]*?<\/style>/g, "");
  let count = 0;
  let m: RegExpExecArray | null;
  NATIVE_TAG_WITH_CLASS_RE.lastIndex = 0;
  while ((m = NATIVE_TAG_WITH_CLASS_RE.exec(markup)) !== null) {
    if (VISUAL_UTILITY_RE.test(m[3])) count++;
  }
  count += countMatches(NATIVE_CONTROL_RE, markup);
  count += countMatches(INLINE_VISUAL_STYLE_RE, markup);
  return count;
}

const listed = new Set(Object.keys(manifest.components));
const problems: string[] = [];
const galleryProblems: string[] = [];

for (const f of onDisk) {
  if (!listed.has(f)) {
    problems.push(`${f}: on disk but missing from .tiers.json`);
  }
}
for (const f of listed) {
  if (!onDisk.has(f)) problems.push(`${f}: in .tiers.json but no such file`);
}

for (const [f, entry] of Object.entries(manifest.components)) {
  const src = onDisk.has(f) ? await Deno.readTextFile(COMPONENTS + f).catch(() => "") : "";

  if (entry.tier === "B") {
    if (!entry.wraps) {
      problems.push(`${f}: tier B must set "wraps" (the View it wraps)`);
    } else if (!sharedViews.has(entry.wraps)) {
      problems.push(
        `${f}: wraps "${entry.wraps}" but no @loca/shared/ui/components/**/${entry.wraps}.svelte exists`,
      );
    } else if (!galleryViews.has(entry.wraps)) {
      galleryProblems.push(
        `${f}: wraps "${entry.wraps}" which is not galleried (add it to GALLERY_VIEWS)`,
      );
    }
  }

  if (entry.tier === "C") {
    // A Tier-C shell is covered when (a) it carries no un-extracted styled
    // markup (the raw-leaf cap, the hard gate below) so every visible leaf is a
    // galleried View/primitive or a thin layout wrapper, and (b) any `composes`
    // it declares for documentation resolves to a galleried, imported View.
    const composes = entry.composes ?? [];
    const orchestratorOf = entry.orchestratorOf ?? [];
    for (const view of composes) {
      if (!sharedViews.has(view)) {
        galleryProblems.push(`${f}: composes "${view}" but no such shared View exists`);
      } else if (!galleryViews.has(view)) {
        galleryProblems.push(
          `${f}: composes "${view}" which is not galleried (add to GALLERY_VIEWS)`,
        );
      }
      // Anti-bypass: a declared View must actually be imported/rendered here.
      if (src && !new RegExp(`[/"']${view}\\.svelte`).test(src)) {
        galleryProblems.push(
          `${f}: composes "${view}" but does not import it (declaration must be honest)`,
        );
      }
    }
    for (const child of orchestratorOf) {
      if (!listed.has(child)) {
        galleryProblems.push(
          `${f}: orchestratorOf "${child}" is not a classified client component`,
        );
      }
    }
    // Raw-markup cap: a shell carrying its own styled leaves is hiding markup
    // that belongs in a shared View. Declared unsharedLeaves are the only escape.
    const styled = src ? countStyledLeaves(src) : 0;
    if (styled > RAW_LEAF_CAP && (entry.unsharedLeaves ?? []).length === 0) {
      galleryProblems.push(
        `${f}: ${styled} styled raw leaves exceed the cap of ${RAW_LEAF_CAP}; extract the markup into a shared View (or declare unsharedLeaves with a reason)`,
      );
    }
  }

  // A Tier-C shell may declare raw styled leaves it owns; each must be a real
  // file (path relative to the client components dir) so a stale entry fails.
  for (const leaf of entry.unsharedLeaves ?? []) {
    if (!onDisk.has(leaf)) {
      problems.push(`${f}: unsharedLeaves entry "${leaf}" is not a client component file`);
    }
  }
}

if (problems.length > 0) {
  console.error(`[ERROR] component-tier manifest issues:`);
  for (const p of problems) console.error(`  ${p}`);
}
if (galleryProblems.length > 0) {
  const label = STRICT_GALLERY ? "ERROR" : "WARN";
  console.error(`[${label}] component-tier gallery-coverage gaps:`);
  for (const p of galleryProblems) console.error(`  ${p}`);
}
if (problems.length > 0 || (STRICT_GALLERY && galleryProblems.length > 0)) {
  Deno.exit(1);
}
