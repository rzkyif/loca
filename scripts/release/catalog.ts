// Release item: the hand-authored @loca/model-catalog compiled into one signed
// catalog.json (whole-payload-minus-signature, like core.json / extension.json)
// and uploaded to R2. Versioned via packages/loca-model-catalog/deno.json;
// diffed by content (the generatedAt timestamp is excluded so a re-build alone
// never counts as a change).

import { ensureDir } from "@std/fs/ensure-dir";
import { join } from "@std/path";
import type { ModelCatalog } from "../../packages/loca-shared/src/domain/catalog.ts";
import { buildCatalogPayload } from "../../packages/loca-model-catalog/src/index.ts";
import {
  type ApplyOpts,
  bumpVersionField,
  canonicalize,
  channelManifestDir,
  colors,
  type DeployEnv,
  DIST_DIR,
  info,
  ok,
  r2Put,
  readVersionField,
  rel,
  type ReleaseChannel,
  type ReleaseItem,
  REPO_ROOT,
  sha256String,
  signEd25519,
  step,
} from "./lib.ts";

const MANIFEST_CACHE_CONTROL = "public, max-age=300";

export const catalogItem: ReleaseItem = {
  id: "catalog",
  label: "model catalog",
  scope: "channel",
  packages: ["catalog"],
  bumpHint: "packages/loca-model-catalog/deno.json (version)",

  version: () => readVersionField(join(REPO_ROOT, "packages/loca-model-catalog/deno.json")),
  versionFile: join(REPO_ROOT, "packages/loca-model-catalog/deno.json"),
  bumpVersion: () => bumpVersionField(join(REPO_ROOT, "packages/loca-model-catalog/deno.json")),

  async sourceHash(_channel: ReleaseChannel): Promise<string> {
    // Hash the catalog content with generatedAt stripped: a fresh build at a
    // new timestamp must not register as a change.
    const payload = buildCatalogPayload("1970-01-01T00:00:00.000Z");
    const { generatedAt: _omit, ...rest } = payload;
    return await sha256String(canonicalize(rest));
  },

  // What `deno task build:catalog` (scripts/catalog/build.ts) writes for local
  // inspection. The build hash-checks it so a wiped/swapped file rebuilds.
  buildOutputs(_channel: ReleaseChannel): Promise<string[]> {
    return Promise.resolve([join(DIST_DIR, "catalog.unsigned.json")]);
  },

  async apply(env: DeployEnv, channel: ReleaseChannel, opts: ApplyOpts): Promise<void> {
    const manifestDir = channelManifestDir(channel);

    step("Compiling + validating catalog");
    const payload = buildCatalogPayload(new Date().toISOString());
    const modelCount = payload.families.reduce((n, f) => n + f.models.length, 0);
    ok(
      `${payload.families.length} families, ${modelCount} models, ` +
        `${payload.stt.models.length} stt models, ${payload.tts.models.length} tts models`,
    );

    step("Signing catalog.json");
    const catalog: ModelCatalog = {
      ...payload,
      signature: await signEd25519(env.signingPrivateKey, payload),
    };
    const manifestPath = join(DIST_DIR, manifestDir, "catalog.json");
    await ensureDir(join(DIST_DIR, manifestDir));
    await Deno.writeTextFile(manifestPath, JSON.stringify(catalog, null, 2));
    ok(`signed catalog.json → ${rel(manifestPath)}`);

    if (opts.dryRun) {
      info(colors.yellow(`dry-run: skipping upload of ${manifestDir}/catalog.json`));
      return;
    }

    step(`Uploading ${manifestDir}/catalog.json to R2`);
    await r2Put(
      env,
      `${manifestDir}/catalog.json`,
      manifestPath,
      "application/json",
      MANIFEST_CACHE_CONTROL,
    );
    ok(`https://${env.storageDomain}/${manifestDir}/catalog.json`);
  },
};
