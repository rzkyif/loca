// Coverage check for shared primitives (A0). Every primitives/*.svelte must:
//   1. be listed in the gallery registry (GALLERY_PRIMITIVES),
//   2. have a sample bundle in @loca/shared/ui/samples/primitives.ts
//      (PRIMITIVE_SAMPLES), and
//   3. actually be rendered by the primitives section (a `P.<Name>` card in
//      Primitives.svelte), so a registered-but-unrendered primitive cannot slip
//      through as a silently missing card.
// This makes "every primitive is visible in the gallery, in each of its states"
// a hard rule, the same way check-view-coverage does for `*View` components.
//
// Wired into `deno task lint` (strict: a gap fails the build).

import { walk } from "@std/fs/walk";
import { fromFileUrl } from "@std/path";
import { GALLERY_PRIMITIVES } from "../../packages/loca-website/src/components/gallery/registry.ts";
import { PRIMITIVE_SAMPLES } from "../../packages/loca-shared/src/ui/samples/primitives.ts";

const STRICT_GALLERY = true;
// Native OS path (fromFileUrl); URL .pathname breaks walk()/readdir on Windows.
const ROOT = fromFileUrl(new URL("../../", import.meta.url));
const PRIMITIVES_DIR = `${ROOT}packages/loca-shared/src/ui/components/primitives/`;
const PRIMITIVES_RENDERER = `${ROOT}packages/loca-website/src/components/gallery/Primitives.svelte`;

const onDisk = new Set<string>();
for await (const e of walk(PRIMITIVES_DIR, { exts: [".svelte"], includeDirs: false })) {
  onDisk.add(e.name.replace(/\.svelte$/, ""));
}

// The renderer aliases PRIMITIVE_SAMPLES to `P` and renders each via `P.<Name>`.
const rendererSrc = await Deno.readTextFile(PRIMITIVES_RENDERER).catch(() => "");

const registry = new Set<string>(GALLERY_PRIMITIVES);
const problems: string[] = [];

for (const name of onDisk) {
  if (!registry.has(name)) {
    problems.push(
      `${name}: not in the gallery registry (add to GALLERY_PRIMITIVES in components/gallery/registry.ts)`,
    );
  }
  if (!(name in PRIMITIVE_SAMPLES)) {
    problems.push(
      `${name}: no samples in @loca/shared/ui/samples/primitives.ts (PRIMITIVE_SAMPLES)`,
    );
  }
  if (!new RegExp(`\\bP\\.${name}\\b`).test(rendererSrc)) {
    problems.push(
      `${name}: registered but the primitives section never renders it (add a \`P.${name}\` card in Primitives.svelte)`,
    );
  }
}
// Reverse: a registry entry with no matching primitive on disk is stale.
for (const name of registry) {
  if (!onDisk.has(name)) {
    problems.push(`${name}: in GALLERY_PRIMITIVES but no primitives/${name}.svelte exists`);
  }
}

if (problems.length > 0) {
  const label = STRICT_GALLERY ? "ERROR" : "WARN";
  console.error(`[${label}] shared primitive coverage gaps:`);
  for (const p of problems) console.error(`  ${p}`);
  if (STRICT_GALLERY) Deno.exit(1);
}
