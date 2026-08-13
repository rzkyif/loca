#!/usr/bin/env -S deno run -A
// Unified release: one entry point (`deno task release` -> latest channel,
// `deno task release:stable` -> stable) that rounds up every release item,
// diffs each against what's published (via the release-state cursor on R2),
// rejects the run if a changed *versioned* item wasn't bumped, shows a plan of
// what will change, asks for a single y/N confirmation, and only then builds +
// uploads the changed items.
//
// Flags:
//   --channel=stable|latest   target channel (the deno tasks set this)
//   --triples=all|host|<csv>  targets to release (default all); host = release:native
//   --yes / -y                skip the confirmation prompt (CI)
//   --force                   ignore the cursor; treat every item as changed
//   --dry-run                 build locally, skip every R2 upload + cursor write
//   --help

import { parseArgs } from "@std/cli/parse-args";
import type { Triple } from "../../packages/loca-shared/src/domain/model.ts";
import {
  colors,
  type DeployEnv,
  fail,
  info,
  loadOrSeedEnv,
  PACKAGES,
  parseChannelFlag,
  type PublishedItem,
  type ReleaseChannel,
  type ReleaseItem,
  runReleasePlan,
  step,
  writeSigningKeys,
} from "./lib.ts";
import { encodeBase64 } from "@std/encoding/base64";
import { ALL_KNOWN_TRIPLES, RELEASE_TARGET_TRIPLES } from "./all-targets.ts";
import type { BuildEnvironment } from "./drivers/mod.ts";
import { podmanLinuxDriver } from "./drivers/podman.ts";
import { windowsUtmDriver } from "./drivers/windows.ts";
import { loadDriverEnv } from "./drivers/config.ts";
import {
  assertReleaseGitState,
  commitVersionBump,
  pushChannelBranch,
  pushMain,
} from "./git-align.ts";
import { coreItem } from "./core.ts";
import { extensionItem } from "./extension.ts";
import { catalogItem } from "./catalog.ts";
import { clientItem } from "./client.ts";
import { androidItem } from "./android.ts";
import { iosItem } from "./ios.ts";
import { appleReleaseConfigured, materializeAppleApiKey } from "./apple-toolchain.ts";
import { schemasItem } from "./schemas.ts";
import { websiteItem } from "./website.ts";

// Apply order: core first (everything else can depend on it being published).
// The landing page is a platform-independent item (no per-triple artifact), so
// it rides along at the tail like the catalog.
const ITEMS: ReleaseItem[] = [
  coreItem,
  extensionItem,
  catalogItem,
  clientItem,
  androidItem,
  iosItem,
  schemasItem,
  websiteItem,
];

// Drift guard: every package a release item claims to be built from must exist
// in the package table. Catches a release item referencing a renamed or removed
// package (the derived-hash items also throw, this covers the documented ones).
for (const item of ITEMS) {
  for (const id of item.packages) {
    if (!(id in PACKAGES)) {
      throw new Error(`release item "${item.id}" references unknown package "${id}"`);
    }
  }
}

interface Flags {
  channel: ReleaseChannel;
  /** Triples the user asked to release for (already validated). */
  requestedTriples: Triple[];
  /** True when releasing for more than just the host triple (the `release`
   *  task; `release:native` stays host-only). Drives the multi-env build path. */
  crossPlatform: boolean;
  yes: boolean;
  force: boolean;
  dryRun: boolean;
  githubRelease: boolean;
  /** Fast-forward + push the channel branch after a successful release, guarded
   *  by strict git preconditions. Set by the full `release` / `release:stable`
   *  tasks; off for `release:native` (a host-only partial release). */
  alignBranches: boolean;
}

function parseFlags(): Flags {
  const args = parseArgs(
    Deno.args.filter((a) => a !== "--"),
    {
      string: ["channel", "triples"],
      boolean: ["yes", "force", "dry-run", "help", "github-release", "align-branches"],
      alias: { y: "yes" },
      default: {
        yes: false,
        force: false,
        "dry-run": false,
        help: false,
        "github-release": false,
        "align-branches": false,
      },
    },
  );
  if (args.help) {
    console.log(`Usage: deno task release[:native][:stable] [flags]

Flags:
  --triples=all|host|<csv>   all targets (default), host only, or a triple list
  --yes, -y                  skip the confirmation prompt
  --force                    ignore the cursor; treat every item as changed
  --dry-run                  build locally; skip R2 uploads + cursor write
  --github-release           also mirror to the rolling per-channel GitHub Release
  --align-branches           fast-forward + push the channel branch after release
  --help`);
    Deno.exit(0);
  }
  // `host` (release:native) builds only this machine's triple. `all` (the
  // default `release` task) and an explicit csv build across targets: the host
  // builds what it can natively and on-demand environments build the rest.
  const spec = (args.triples ?? "all").trim();
  let requestedTriples: Triple[];
  if (spec === "host") {
    requestedTriples = [Deno.build.target as Triple];
  } else if (spec === "all") {
    requestedTriples = RELEASE_TARGET_TRIPLES;
  } else {
    requestedTriples = spec.split(",").map((t) => t.trim()) as Triple[];
    for (const t of requestedTriples) {
      if (!ALL_KNOWN_TRIPLES.includes(t)) {
        fail(`unknown --triples entry "${t}". Valid: ${ALL_KNOWN_TRIPLES.join(", ")}, host, all`);
      }
    }
  }
  const crossPlatform = !(
    requestedTriples.length === 1 && requestedTriples[0] === (Deno.build.target as Triple)
  );
  return {
    channel: parseChannelFlag(args.channel),
    requestedTriples,
    crossPlatform,
    yes: args.yes,
    force: args.force,
    dryRun: args["dry-run"],
    githubRelease: args["github-release"],
    alignBranches: args["align-branches"],
  };
}

async function main(): Promise<void> {
  const flags = parseFlags();
  const mode = flags.crossPlatform ? "all targets" : "host only";
  console.log(colors.bold(`\nloca release: ${flags.channel} channel (${mode})\n`));

  // Branch-aligned release: enforce the git preconditions up front, before any
  // env load or build, so a dirty/misaligned tree fails fast on a pristine tree.
  if (flags.alignBranches) {
    await assertReleaseGitState(flags.channel);
  }

  // Cross-platform runs pass on-demand build environments; the core item routes
  // each triple to the host (its own OS) or a driver (started for the build, then
  // stopped), and reports what it couldn't build. The host-only path
  // (release:native) passes no environments and builds just the host triple.
  const environments: BuildEnvironment[] | undefined = flags.crossPlatform
    ? [podmanLinuxDriver, windowsUtmDriver]
    : undefined;
  // Promote the drivers' device-specific config from .env into the process env
  // before any driver runs (its cfg() reads them). Only the cross-platform path
  // uses drivers; the host-only run skips it.
  if (environments) await loadDriverEnv();

  step("Loading deploy environment");
  const env = await loadOrSeedEnv();
  // APPLE_API_KEY_B64 is primarily the CI form of the App Store Connect key,
  // but hasAppStoreConnectApi accepts either form, so honor the base64 here too
  // (no-op when it is unset or APPLE_API_KEY_PATH already points at the .p8).
  const cleanupApiKey = await materializeAppleApiKey(env);
  try {
    await releaseWithEnv(env, flags, environments);
  } finally {
    await cleanupApiKey();
  }
}

async function releaseWithEnv(
  env: DeployEnv,
  flags: Flags,
  environments: BuildEnvironment[] | undefined,
): Promise<void> {
  await writeSigningKeys(encodeBase64(env.signingPublicKey));

  // The client can only be released when the Tauri updater keys are present,
  // and the android APK only when its signing keystore is present; drop either
  // from the run (with a warning) rather than marking it released.
  const items = ITEMS.filter((it) => {
    if (it.id === "client" && (!env.tauriUpdaterPublicKey || !env.tauriUpdaterPrivateKey)) {
      info(colors.yellow(`Tauri updater keys not set in .env; skipping the client this run.`));
      return false;
    }
    if (it.id === "android" && !env.androidKeystoreB64) {
      info(colors.yellow(`Android keystore not set in .env; skipping the android APK this run.`));
      return false;
    }
    if (it.id === "ios" && !appleReleaseConfigured(env)) {
      info(
        colors.yellow(
          `Apple signing / App Store Connect not configured in .env; skipping the iOS client this run.`,
        ),
      );
      return false;
    }
    return true;
  });

  // The version files runReleasePlan bumps for a latest-channel release; committed
  // and pushed to `main` below. `origin/latest` is left at the released (pre-bump)
  // commit, matching the cursor's pre-bump hash.
  const publishedItems: PublishedItem[] = [];

  const published = await runReleasePlan(env, items, flags.channel, {
    yes: flags.yes,
    force: flags.force,
    dryRun: flags.dryRun,
    triples: flags.requestedTriples,
    environments,
    githubRelease: flags.githubRelease,
    // A stable release is a fast-forward promotion of already-bumped versions
    // from latest, so it must NOT bump (noBump). A latest release DOES bump: the
    // bump is committed + pushed to `main` below, while `latest` stays at the
    // released (pre-bump) commit that matches the cursor's pre-bump hash. A
    // host-only release:native keeps its uncommitted bump (no branch align).
    noBump: flags.alignBranches && flags.channel === "stable",
    publishedOut: publishedItems,
  });

  if (published > 0 && !flags.dryRun) {
    console.log(
      "\n" +
        colors.green(colors.bold(`✓ release complete (${flags.channel})`)) +
        `  ${published} item(s) published\n`,
    );
  }

  // Align branches to what was just published. Only after a real, non-dry release
  // that published something: an abort or nothing-to-do returns 0 and leaves the
  // branches untouched.
  //
  // The channel branch points at the RELEASED commit (un-bumped); the post-release
  // bump lands on `main` only. So fast-forward the channel branch to the released
  // HEAD FIRST - the bump is still uncommitted in the working tree at this point -
  // then, for latest, commit the bump and push it to `main`. Source hashes are
  // version-blind, so the bump on `main` never re-runs the release on its own; it
  // ships bundled with the next real source change on a later promotion. A stable
  // release is a promotion that did not bump and does not touch `main`.
  if (flags.alignBranches && !flags.dryRun && published > 0) {
    await pushChannelBranch(flags.channel);
    if (flags.channel === "latest") {
      await commitVersionBump(publishedItems.filter((p) => p.sourceChanged));
      await pushMain();
    }
  }
}

if (import.meta.main) {
  try {
    await main();
  } catch (err) {
    fail(err instanceof Error ? err.message : String(err));
  }
}
