#!/usr/bin/env -S deno run -A
// build:catalog: validate the hand-authored @loca/model-catalog families and
// write an UNSIGNED catalog.json to dist/ for local inspection. No keys, no
// upload. Use `deno task release` to sign + publish.

import { fromFileUrl, join } from "@std/path";
import { ensureDir } from "@std/fs/ensure-dir";
import { buildCatalogPayload } from "../../packages/loca-model-catalog/src/index.ts";

const REPO_ROOT = fromFileUrl(new URL("../..", import.meta.url));
const OUT = join(REPO_ROOT, "dist", "catalog.unsigned.json");

const payload = buildCatalogPayload(new Date().toISOString());
const models = payload.families.flatMap((f) => f.models);
await ensureDir(join(REPO_ROOT, "dist"));
await Deno.writeTextFile(OUT, JSON.stringify(payload, null, 2));

console.log(
  `✓ catalog valid: ${payload.families.length} families, ${models.length} models, ` +
    `${payload.stt.models.length} stt models, ${payload.stt.presets.length} stt presets`,
);
for (const f of payload.families) {
  console.log(`  ${f.family}: ${f.models.map((m) => m.name).join(", ")}`);
}
console.log(`  stt: ${payload.stt.models.map((m) => m.name).join(", ")}`);
console.log(`→ ${OUT}`);
