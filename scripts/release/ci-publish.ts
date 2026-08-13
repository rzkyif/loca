#!/usr/bin/env -S deno run -A
// CI publish half: the single "host" that runs AFTER every ci-build runner. It
// downloads each runner's staging dir (one per artifact, under --staging-root),
// reconstructs dist/, and composes + Ed25519-signs + uploads the unified
// manifests ONCE - exactly the role the local all-targets host plays, but fed by
// pre-built bundles/descriptors instead of build-environment drivers. It then
// mirrors the run to the rolling per-channel GitHub Release.
//
// It plans against the SAME R2 release-state cursor the local pipeline uses, so a
// CI run and a local run can never double-publish. Version bumps live on `main`
// (the channel branches are clean fast-forwards), so this never writes the repo:
// --noBump records the as-built source hash, keeping a re-push idempotent.
//
// Flags:
//   --channel=stable|latest    target channel (required; the workflow sets it)
//   --staging-root=<path>      dir holding the downloaded per-runner stage dirs
//   --yes / -y                 skip the confirmation prompt (always set in CI)
//   --dry-run                  compose + sign locally; no R2 upload, cursor, or GH
//   --no-github-release        skip the GitHub Release mirror (R2 only)

import { parseArgs } from "@std/cli/parse-args";
import { ensureDir } from "@std/fs/ensure-dir";
import { walk } from "@std/fs/walk";
import { dirname, join, relative } from "@std/path";
import { encodeBase64 } from "@std/encoding/base64";
import {
  colors,
  DIST_DIR,
  exists,
  fail,
  info,
  loadOrSeedEnv,
  ok,
  parseChannelFlag,
  type ReleaseItem,
  runReleasePlan,
  step,
  writeSigningKeys,
} from "./lib.ts";
import { RELEASE_TARGET_TRIPLES } from "./all-targets.ts";
import {
  ANDROID_DESCRIPTOR_FILENAME,
  BUNDLE_FILENAME,
  CLIENT_DESCRIPTOR_FILENAME,
  type PrebuiltStaging,
  readAndroidDescriptor,
  readBundle,
  readClientDescriptor,
} from "./artifacts.ts";
import { coreItem } from "./core.ts";
import { uploadInstallers } from "./core-installers.ts";
import { extensionItem } from "./extension.ts";
import { catalogItem } from "./catalog.ts";
import { clientItem } from "./client.ts";
import { androidItem } from "./android.ts";
import { schemasItem } from "./schemas.ts";
import { websiteItem } from "./website.ts";

/** Copy every file under a runner's `dist/` into the host's DIST_DIR, preserving
 *  the dist-relative layout, so the descriptors' relPaths re-anchor + verify. */
async function mergeDist(stageDir: string): Promise<number> {
  const src = join(stageDir, "dist");
  if (!(await exists(src))) return 0;
  let n = 0;
  for await (const entry of walk(src, { includeDirs: false })) {
    const rel = relative(src, entry.path);
    const dest = join(DIST_DIR, rel);
    await ensureDir(dirname(dest));
    await Deno.copyFile(entry.path, dest);
    n++;
  }
  return n;
}

/** Walk the downloaded staging root (one subdir per runner artifact),
 *  reconstruct DIST_DIR, and collect the core bundles + client/android
 *  descriptors into the PrebuiltStaging the items consume. */
async function collectPrebuilt(stagingRoot: string): Promise<PrebuiltStaging> {
  const staging: PrebuiltStaging = { coreBundles: [], clientDescriptors: [] };
  // A website-only / platform-independent-only publish skips the build matrix,
  // so no runner staged anything and the download leaves no staging root. That
  // is expected: the publish plan just builds the changed coordinator-side items
  // (catalog, schemas, landing page) with an empty prebuilt set.
  if (!(await exists(stagingRoot))) {
    info(`no staging root (${stagingRoot}); publishing platform-independent items only`);
    return staging;
  }
  for await (const dir of Deno.readDir(stagingRoot)) {
    if (!dir.isDirectory) continue;
    const stageDir = join(stagingRoot, dir.name);
    const copied = await mergeDist(stageDir);
    info(`${dir.name}: merged ${copied} file(s) into dist/`);

    if (await exists(join(stageDir, BUNDLE_FILENAME))) {
      staging.coreBundles.push(await readBundle(stageDir));
    }
    if (await exists(join(stageDir, CLIENT_DESCRIPTOR_FILENAME))) {
      staging.clientDescriptors.push(await readClientDescriptor(stageDir));
    }
    if (await exists(join(stageDir, ANDROID_DESCRIPTOR_FILENAME))) {
      if (staging.android) fail(`more than one android descriptor in the staging root`);
      staging.android = await readAndroidDescriptor(stageDir);
    }
  }
  return staging;
}

async function main(): Promise<void> {
  const args = parseArgs(
    Deno.args.filter((a) => a !== "--"),
    {
      string: ["channel", "staging-root"],
      boolean: ["yes", "dry-run", "github-release"],
      alias: { y: "yes" },
      default: { yes: false, "dry-run": false, "github-release": true },
    },
  );
  const channel = parseChannelFlag(args.channel);
  const stagingRoot = args["staging-root"]
    ? String(args["staging-root"])
    : join(DIST_DIR, "staging");

  console.log(colors.bold(`\nloca ci-publish: ${channel} channel\n`));

  step("Loading deploy environment");
  const env = await loadOrSeedEnv();
  await writeSigningKeys(encodeBase64(env.signingPublicKey));

  step("Collecting pre-built artifacts");
  const prebuilt = await collectPrebuilt(stagingRoot);
  ok(
    `core bundles: ${prebuilt.coreBundles.length}, client: ${prebuilt.clientDescriptors.length}, ` +
      `android: ${prebuilt.android ? prebuilt.android.apks.length + " APK(s)" : "none"}`,
  );

  // Core + the platform-independent items always run (the latter build directly
  // on this host); client/android only when a runner actually staged them. Each
  // is diffed against the cursor, so an unchanged item is a no-op - a website-only
  // publish (no staged bundles) simply applies the landing page and nothing else.
  const items: ReleaseItem[] = [coreItem, extensionItem, catalogItem];
  if (prebuilt.clientDescriptors.length > 0) items.push(clientItem);
  if (prebuilt.android) items.push(androidItem);
  items.push(schemasItem, websiteItem);

  // Publish the native installers each runner built (the unified Windows .exe /
  // macOS .pkg + the Linux Core deb/rpm) + a signed installers.json BEFORE the
  // release plan, so the installers ride into the same GitHub-Release mirror as
  // every other artifact: uploadInstallers writes installers.json{,.sig} under
  // dist/<manifestDir> (picked up by the mirror's manifest walk) and returns the
  // installer binaries as GH assets, which we hand the plan via
  // extraGithubAssets. The installers are auxiliary download assets, so they
  // ride outside the release-plan cursor. R2 puts are idempotent, so a plan
  // failure after this just retries on re-push.
  const installers = prebuilt.coreBundles.flatMap((b) => b.installers ?? []);
  let extraGithubAssets: Array<{ path: string; name: string }> = [];
  if (installers.length > 0) {
    const coreVersion =
      prebuilt.coreBundles.find((b) => b.installers?.length)?.version ??
      prebuilt.coreBundles[0].version;
    const clientVersion = prebuilt.clientDescriptors[0]?.version;
    extraGithubAssets = await uploadInstallers(
      env,
      channel,
      { coreVersion, clientVersion },
      installers,
      {
        triples: RELEASE_TARGET_TRIPLES,
        dryRun: args["dry-run"],
      },
    );
  }

  const published = await runReleasePlan(env, items, channel, {
    yes: args.yes,
    force: false,
    dryRun: args["dry-run"],
    triples: RELEASE_TARGET_TRIPLES,
    prebuilt,
    noBump: true,
    githubRelease: args["github-release"] && !args["dry-run"],
    extraGithubAssets,
  });

  if (published > 0 && !args["dry-run"]) {
    console.log(
      "\n" +
        colors.green(colors.bold(`✓ ci-publish complete (${channel})`)) +
        `  ${published} item(s) published\n`,
    );
  }
}

if (import.meta.main) {
  try {
    await main();
  } catch (err) {
    fail(err instanceof Error ? err.message : String(err));
  }
}
