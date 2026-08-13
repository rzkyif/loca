#!/usr/bin/env -S deno run -A
// CI post-publish version bump: the committed counterpart to the local release's
// auto-bump. It runs as the final job of the Release workflow, AFTER ci-publish
// has published the latest channel. It bumps the just-published items' version
// files and commits them to `main` ONLY. It does NOT touch `latest` and does NOT
// write the cursor: `latest` stays at the released commit (the codebase as
// actually shipped, un-bumped), and the shared R2 cursor keeps the as-released
// source hash that ci-publish already recorded.
//
// Latest channel only: a stable release is a fast-forward promotion of
// already-bumped versions, so there is nothing to bump (the workflow only runs
// this job on the latest branch; this script also no-ops on any other channel).
//
// Which items to bump: after ci-publish's write, a just-published item's recorded
// cursor version equals its local version (the version file has not advanced yet).
// Every other item was bumped after its last release, so its local version is
// strictly greater. So "recorded.version === local version" selects exactly the
// items this release published and left un-bumped. Source hashes are version-BLIND
// (each item strips its version before hashing) and client/android extraChanged
// never compares against the LOCAL version (platform fill + live-vs-live installer
// staleness only), so ci-publish only ever publishes items whose source actually
// changed (or a genuine same-version fill/refresh) - never a lone version bump.
// That makes
// this selection equivalent to the local auto-bump's "sourceChanged only" filter:
// bumping the just-published items is correct and cannot churn.
//
// No re-release loop: the bump lands on `main`, and a lone version bump no longer
// re-triggers a release, because the version is stripped out of every source hash.
// The pending bump sits on `main` until a REAL source change lands, then ships
// bundled with it on the next `main -> latest` promotion (the higher version clears
// the version-bump gate, which still rejects a source change WITHOUT a bump). This
// job's push is via GITHUB_TOKEN, which GitHub does not let trigger another
// workflow. A non-fast-forward pushMain (a genuine concurrent push during the job)
// still fails loudly.
//
// Flags:
//   --channel=stable|latest    target channel (the workflow sets it from branch)
//   --dry-run                  bump + commit locally; no push

import { parseArgs } from "@std/cli/parse-args";
import {
  bumpPatch,
  colors,
  fail,
  info,
  loadOrSeedEnv,
  ok,
  parseChannelFlag,
  type PublishedItem,
  readReleaseCursor,
  type ReleaseChannel,
  type ReleaseItem,
  step,
} from "./lib.ts";
import { commitVersionBump, pushMain } from "./git-align.ts";
import { coreItem } from "./core.ts";
import { extensionItem } from "./extension.ts";
import { catalogItem } from "./catalog.ts";
import { clientItem } from "./client.ts";
import { androidItem } from "./android.ts";
import { schemasItem } from "./schemas.ts";
import { websiteItem } from "./website.ts";

// The same set ci-preflight / ci-publish plan against (iOS ships through the App
// Store, not the R2 cursor, so it is not here).
const ITEMS: ReleaseItem[] = [
  coreItem,
  extensionItem,
  catalogItem,
  clientItem,
  androidItem,
  schemasItem,
  websiteItem,
];

function recordedFor(
  cursor: Awaited<ReturnType<typeof readReleaseCursor>>,
  item: ReleaseItem,
  channel: ReleaseChannel,
) {
  return item.scope === "shared" ? cursor.shared[item.id] : cursor.channels[channel]?.[item.id];
}

async function main(): Promise<void> {
  const args = parseArgs(
    Deno.args.filter((a) => a !== "--"),
    { string: ["channel"], boolean: ["dry-run"], default: { "dry-run": false } },
  );
  const channel = parseChannelFlag(args.channel);
  const dryRun = args["dry-run"];
  console.log(colors.bold(`\nloca ci-bump: ${channel} channel\n`));

  if (channel !== "latest") {
    ok(`nothing to bump on the ${channel} channel (promotions carry bumped versions)`);
    return;
  }

  step("Loading deploy environment");
  const env = await loadOrSeedEnv();

  step("Reading release-state cursor");
  const cursor = await readReleaseCursor(env);

  // Select the just-published, un-bumped items (recorded version === local).
  const selected: Array<{ item: ReleaseItem; recordedVersion: string }> = [];
  for (const item of ITEMS) {
    const recorded = recordedFor(cursor, item, channel);
    if (!recorded?.version) continue;
    const local = await item.version();
    if (local === recorded.version) selected.push({ item, recordedVersion: recorded.version });
  }

  if (selected.length === 0) {
    ok(`no just-published items to bump; ${channel} is already bumped`);
    return;
  }

  // Preview only: report what would bump without touching the tree or `main`
  // (bumpPatch mirrors bumpVersion's patch increment).
  if (dryRun) {
    step("dry-run: version bump preview (no writes)");
    const seen = new Set<string>();
    for (const { item, recordedVersion } of selected) {
      if (seen.has(item.versionFile)) continue;
      seen.add(item.versionFile);
      info(`would bump ${item.label} ${recordedVersion} -> ${bumpPatch(recordedVersion)}`);
    }
    info(colors.yellow("dry-run: skipping the bump commit and the main push"));
    return;
  }

  // Bump each selected item's version file (dedupe by file so a shared file like
  // tauri.conf.json bumps once), then commit as `bump: <labels>`.
  step("Bumping versions for the next release");
  const bumpedFiles = new Set<string>();
  const bumped: PublishedItem[] = [];
  for (const { item, recordedVersion } of selected) {
    if (!bumpedFiles.has(item.versionFile)) {
      bumpedFiles.add(item.versionFile);
      const next = await item.bumpVersion();
      ok(`bumped ${item.label} ${recordedVersion} -> ${next}`);
    }
    bumped.push({
      id: item.id,
      label: item.label,
      versionFile: item.versionFile,
      sourceChanged: true,
      version: recordedVersion,
    });
  }

  await commitVersionBump(bumped);

  // Persist the bump on `main` only. `latest` is left at the released commit and
  // the cursor keeps ci-publish's pre-bump hash, so the bump becomes the next
  // thing a `main -> latest` promotion publishes (see the header note).
  await pushMain();
}

if (import.meta.main) {
  try {
    await main();
  } catch (err) {
    fail(err instanceof Error ? err.message : String(err));
  }
}
