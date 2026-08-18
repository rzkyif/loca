#!/usr/bin/env -S deno run -A
// CI publish half: the single "host" that runs AFTER every ci-build runner. It
// downloads each runner's staging dir (one per artifact, under --staging-root),
// reconstructs dist/, and composes + Ed25519-signs + uploads the unified
// manifests ONCE, fed by the pre-built bundles/descriptors the matrix staged. It
// then mirrors the run to the rolling per-channel GitHub Release.
//
// It plans against the R2 release-state cursor, so re-pushing the same commit is
// a no-op. Version bumps live on `main` (the channel branches are clean
// fast-forwards), so this never writes the repo: the cursor records the as-built
// source hash and ci-bump.ts commits the bump separately.
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
  PACKAGES,
  parseChannelFlag,
  type ReleaseItem,
  runReleasePlan,
  step,
  writeSigningKeys,
} from "./lib.ts";
import {
  ANDROID_DESCRIPTOR_FILENAME,
  BUNDLE_FILENAME,
  CLIENT_DESCRIPTOR_FILENAME,
  IOS_RECEIPT_FILENAME,
  type PrebuiltStaging,
  readAndroidDescriptor,
  readBundle,
  readClientDescriptor,
  readIosReceipt,
} from "./artifacts.ts";
import { coreItem } from "./core.ts";
import { uploadInstallers } from "./core-installers.ts";
import { extensionItem } from "./extension.ts";
import { catalogItem } from "./catalog.ts";
import { clientItem } from "./client.ts";
import { androidItem } from "./android.ts";
import { iosItem } from "./ios.ts";
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
    if (await exists(join(stageDir, IOS_RECEIPT_FILENAME))) {
      if (staging.ios) fail(`more than one ios receipt in the staging root`);
      staging.ios = await readIosReceipt(stageDir);
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
      `android: ${prebuilt.android ? prebuilt.android.apks.length + " APK(s)" : "none"}, ` +
      `ios: ${prebuilt.ios ? prebuilt.ios.version : "none"}`,
  );

  // Core + the platform-independent items always run (the latter build directly
  // on this host); client/android/ios only when a runner actually staged them.
  // Each is diffed against the cursor, so an unchanged item is a no-op - a
  // website-only publish (no staged bundles) simply applies the landing page and
  // nothing else. iOS stages a receipt rather than an artifact: the macOS runner
  // already uploaded the .ipa, so the item only records the cursor entry.
  const items: ReleaseItem[] = [coreItem, extensionItem, catalogItem];
  if (prebuilt.clientDescriptors.length > 0) items.push(clientItem);
  if (prebuilt.android) items.push(androidItem);
  if (prebuilt.ios) items.push(iosItem);
  items.push(schemasItem, websiteItem);

  // Drift guard: every package a release item claims to be built from must exist
  // in the package table. Catches a release item referencing a renamed or removed
  // package (the derived-hash items also throw, this covers the documented ones).
  for (const item of items) {
    for (const id of item.packages) {
      if (!(id in PACKAGES)) {
        fail(`release item "${item.id}" references unknown package "${id}"`);
      }
    }
  }

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
      { dryRun: args["dry-run"] },
    );
  }

  const published = await runReleasePlan(env, items, channel, {
    yes: args.yes,
    dryRun: args["dry-run"],
    prebuilt,
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
