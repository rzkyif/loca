// Coverage check for shared `*View` components. Every View must:
//   1. have NON-EMPTY named samples in @loca/shared/ui/samples (the SAMPLES
//      registry) - at least one scenario, so the card is never blank,
//   2. be listed in the gallery registry (GALLERY_VIEWS) AND actually rendered
//      by the gallery (a `SAMPLES.<Name>` reference in Gallery.svelte or
//      MobileGallery.svelte), and
//   3. be wrapped by at least one client component, or composed by another
//      shared View (the single-source rule: a View only the website uses is a
//      re-implementation in disguise).
// See packages/loca-shared/src/ui/README.md and packages/loca-website/AGENTS.md.
//
// The renderer is hand-authored (one card block per View), not a blind iteration
// of the registry, so listing a View in GALLERY_VIEWS is not enough: this walker
// also asserts the renderer references it, closing the gap where a registered
// View is silently never rendered.
//
// EMBEDDED_VIEWS are the exception: a pure structural sub-piece always rendered
// inside one parent View, whose card already shows it, earns coverage
// transitively instead of via a redundant card of its own. Such a View is exempt
// from requirements 1-2; instead its declared parent must be galleried, rendered,
// and must actually render the child (so it can never silently disappear). The
// askuser question sub-views are the analogous hardcoded case below. Wired into
// `deno task lint`.

import { walk } from "@std/fs/walk";
import { fromFileUrl } from "@std/path";
import {
  EMBEDDED_VIEWS,
  GALLERY_VIEWS,
} from "../../packages/loca-website/src/components/gallery/registry.ts";
import { SAMPLES } from "../../packages/loca-shared/src/ui/samples/index.ts";

const STRICT = true;
// Native OS path (fromFileUrl); URL .pathname breaks walk()/readdir on Windows.
const ROOT = fromFileUrl(new URL("../../", import.meta.url));
const VIEWS_DIR = `${ROOT}packages/loca-shared/src/ui/components/`;
const GALLERY_DIR = `${ROOT}packages/loca-website/src/components/gallery/`;
const CLIENT_COMPONENTS = `${ROOT}packages/loca-client/src/ui/components/`;

async function viewNames(): Promise<string[]> {
  const names: string[] = [];
  // Walk recursively: Views live in domain subfolders (chat/messages, settings,
  // ...), so match on the file's basename, not its subfolder path.
  for await (const e of walk(VIEWS_DIR, { exts: [".svelte"], includeDirs: false })) {
    const base = e.name.replace(/\.svelte$/, "");
    if (base.endsWith("View")) names.push(base);
  }
  return names.sort();
}

async function read(path: string): Promise<string> {
  try {
    return await Deno.readTextFile(path);
  } catch {
    return "";
  }
}

async function concat(dir: string, skip?: (p: string) => boolean): Promise<string> {
  let all = "";
  for await (const e of walk(dir, { exts: [".svelte"], includeDirs: false })) {
    if (skip?.(e.path)) continue;
    all += await Deno.readTextFile(e.path);
  }
  return all;
}

const [views, galleryRenderSrc, clientSrc, sharedSrc] = await Promise.all([
  viewNames(),
  // The hand-authored renderer: the desktop gallery plus the mobile section.
  Promise.all(
    [`${GALLERY_DIR}Gallery.svelte`, `${GALLERY_DIR}MobileGallery.svelte`].map(read),
  ).then((parts) => parts.join("")),
  concat(CLIENT_COMPONENTS),
  concat(VIEWS_DIR),
]);

const sampleBundles = SAMPLES as Record<string, Record<string, unknown>>;
const galleryViews = new Set<string>(GALLERY_VIEWS);
const problems: string[] = [];
// `[/"']${v}.svelte` anchors on a path boundary (`/`, quote) before the name so a
// longer View name that contains a shorter one as a substring can't false-match.
const referenced = (v: string, src: string) => new RegExp(`[/"']${v}\\.svelte`).test(src);
for (const v of views) {
  const embeddedParent = EMBEDDED_VIEWS[v];
  if (embeddedParent) {
    // Transitively covered: no card of its own, so skip the sample + render-card
    // requirements and instead prove the parent shows it. A View must not be in
    // both registries (it would demand a card it deliberately does not have).
    if (galleryViews.has(v)) {
      problems.push(`${v}: in both GALLERY_VIEWS and EMBEDDED_VIEWS (pick one)`);
    }
    if (!galleryViews.has(embeddedParent)) {
      problems.push(
        `${v}: embedded parent "${embeddedParent}" is not galleried (add it to GALLERY_VIEWS)`,
      );
    } else if (!new RegExp(`SAMPLES\\.${embeddedParent}\\b`).test(galleryRenderSrc)) {
      problems.push(
        `${v}: embedded parent "${embeddedParent}" is not rendered by the gallery (no \`SAMPLES.${embeddedParent}\` card)`,
      );
    }
    // Honesty: the child must actually be rendered under the parent - either a
    // shared View imports it (the parent composes it directly) or the gallery
    // renderer supplies it inside the parent's card (a snippet-fed child).
    if (!referenced(v, sharedSrc) && !referenced(v, galleryRenderSrc)) {
      problems.push(
        `${v}: declared embedded in "${embeddedParent}" but nothing renders it (no \`${v}.svelte\` in the shared Views or the gallery renderer)`,
      );
    }
    continue;
  }
  // A non-empty sample bundle in the SAMPLES registry (a `{}` bundle would pass
  // a key-existence check yet render zero cards).
  const bundle = sampleBundles[v];
  if (!bundle) {
    problems.push(`${v}: no samples in @loca/shared/ui/samples (add to SAMPLES)`);
  } else if (Object.keys(bundle).length === 0) {
    problems.push(`${v}: its SAMPLES bundle is empty (add at least one scenario)`);
  }
  if (!galleryViews.has(v)) {
    problems.push(
      `${v}: not in the gallery registry (add to GALLERY_VIEWS in components/gallery/registry.ts, or EMBEDDED_VIEWS if a parent card shows it)`,
    );
  } else if (!new RegExp(`SAMPLES\\.${v}\\b`).test(galleryRenderSrc)) {
    // Listed but the hand-authored renderer never renders it: a silently broken
    // gallery entry. The `\b` anchor stops a longer View name (`SAMPLES.FooBarView`)
    // from satisfying a shorter prefix (`Foo`). Add a card block in Gallery.svelte
    // (or MobileGallery.svelte).
    problems.push(
      `${v}: in GALLERY_VIEWS but the gallery never renders it (no \`SAMPLES.${v}\` card in Gallery.svelte/MobileGallery.svelte)`,
    );
  }
  // Covered if a client component wraps it directly, or another shared View
  // composes it (so it still ships to the client transitively). A View that is
  // neither is website-only: the single-source drift risk AGENTS.md warns about.
  if (!referenced(v, clientSrc) && !referenced(v, sharedSrc)) {
    problems.push(
      `${v}: no client component wraps it and no shared View composes it (single-source rule)`,
    );
  }
}

// Reverse: every registry entry must be a real on-disk View, so a stale name
// (e.g. after a rename) fails instead of silently rendering nothing. The same
// applies to every EMBEDDED_VIEWS parent (child existence is covered by the main
// loop above, which walks every on-disk View).
const onDiskViews = new Set(views);
for (const v of galleryViews) {
  if (!onDiskViews.has(v)) {
    problems.push(`${v}: in GALLERY_VIEWS but no @loca/shared/ui/components/**/${v}.svelte exists`);
  }
}
for (const parent of Object.values(EMBEDDED_VIEWS)) {
  if (!onDiskViews.has(parent)) {
    problems.push(
      `${parent}: named as an EMBEDDED_VIEWS parent but no @loca/shared/ui/components/**/${parent}.svelte exists`,
    );
  }
}

// The askUser question sub-views (chat/messages/askuser/*) are tightly coupled
// internals of AskUserFormView (callbacks + draft state), not standalone Views,
// so they earn coverage transitively: AskUserFormView is galleried and the
// askUserForm samples exercise each question kind. Enforce that every one is
// actually imported by AskUserFormView, so a new question kind cannot be added
// without being wired into (and thus shown by) the galleried parent.
const ASKUSER_DIR = `${VIEWS_DIR}chat/messages/askuser/`;
const askUserFormSrc = await read(`${VIEWS_DIR}chat/userinput/AskUserFormView.svelte`);
for await (const e of walk(ASKUSER_DIR, { exts: [".svelte"], includeDirs: false })) {
  const base = e.name.replace(/\.svelte$/, "");
  if (!new RegExp(`[/"']${base}\\.svelte`).test(askUserFormSrc)) {
    problems.push(
      `${base}: askUser question not imported by AskUserFormView (it would never render)`,
    );
  }
}

if (problems.length > 0) {
  const label = STRICT ? "ERROR" : "WARN";
  console.error(`[${label}] shared View coverage gaps:`);
  for (const p of problems) console.error(`  ${p}`);
  if (STRICT) Deno.exit(1);
}
