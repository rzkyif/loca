#!/usr/bin/env -S deno run -A
// Unified build: compiles every local artifact whose source changed since the
// last build, and skips the rest.
//
// Idempotency comes from a dist/.build-state.json cursor recording each item's
// {source, output} hashes. The source hash reuses the very same sourceHash()
// the release system uses to detect changes (so "needs building" and "needs
// releasing" can't drift apart). The output hash is taken over the item's
// compiled artifacts (the paths its buildOutputs() reports). An item rebuilds
// when either differs from the cursor, so a wiped dist/ or a swapped artifact
// forces a rebuild instead of a false "up to date". `deno task clean` removes
// dist/, which clears the cursor and forces a full rebuild; --force does the
// same without cleaning.
//
// Builds (core + helpers, client, catalog, android, ios) for the HOST triple.
// Every desktop platform is built natively, so a full multi-platform build is
// the CI matrix's job (.github/workflows/release.yml), not this script's.
//
// A subset of the release item set: android is included (gated on the keystore
// being present), ios is included (gated on Apple signing being configured and a
// mac host; build-only, never uploads), and the landing page is NOT - it has no
// local compile step here; its release item builds Astro itself at release time
// (via `deno task build:website` under the hood). The extension tarball and the
// schemas likewise have no compile step, so they're release-only.
//
// Flags:
//   --channel=stable|latest   channel for the core/client builds (default latest)
//   --force                   rebuild every item, ignoring the cursor
//   --help

import { parseArgs } from "@std/cli/parse-args";
import { ensureDir } from "@std/fs/ensure-dir";
import { load as loadDotenv } from "@std/dotenv";
import { join } from "@std/path";
import {
  channelBinSuffix,
  colors,
  type DeployEnv,
  detectHostTriple,
  DIST_DIR,
  ENV_PATH,
  exists,
  fail,
  hashPaths,
  info,
  loadOrSeedEnv,
  ok,
  parseChannelFlag,
  type ReleaseChannel,
  type ReleaseItem,
  REPO_ROOT,
  step,
} from "./release/lib.ts";
import { buildCoreArtifacts, coreItem } from "./release/core.ts";
import { clientItem } from "./release/client.ts";
import { catalogItem } from "./release/catalog.ts";
import { androidItem, buildAndroidBundle } from "./release/android.ts";
import { androidToolchainReady } from "./release/android-toolchain.ts";
import { buildIosBundle, iosItem } from "./release/ios.ts";
import { iosToolchainReady } from "./release/apple-toolchain.ts";

const BUILD_STATE = join(DIST_DIR, ".build-state.json");

interface Buildable {
  /** Cursor key. Channel-specific items (core/client) include the channel so a
   *  latest build and a stable build are tracked (and skipped) independently. */
  key: string;
  label: string;
  sourceHash(): Promise<string>;
  /** Content hash of the compiled artifacts on disk. Missing artifacts hash
   *  differently (hashPaths skips absent paths), so a wiped or swapped output
   *  no longer matches the cursor and triggers a rebuild. */
  outputHash(): Promise<string>;
  /** Args to `deno`, run from the repo root (host-only builds). Mutually
   *  exclusive with `run`. */
  cmd?: string[];
  /** In-process build (cross-platform core: drives the host + driver
   *  environments via buildCoreUnified). Mutually exclusive with `cmd`. */
  run?: () => Promise<void>;
}

/** {source, output} hashes recorded after each item's last successful build. */
interface BuildRecord {
  source: string;
  output: string;
}
type BuildState = Record<string, BuildRecord>;

interface Flags {
  channel: ReleaseChannel;
  force: boolean;
}

function parseFlags(): Flags {
  const args = parseArgs(
    Deno.args.filter((a) => a !== "--"),
    {
      string: ["channel"],
      boolean: ["force", "help"],
      default: { force: false, help: false },
    },
  );
  if (args.help) {
    console.log(`Usage: deno task build[:stable] [flags]

Flags:
  --channel=stable|latest   channel for the core/client builds (default latest)
  --force                   rebuild every item, ignoring the cursor
  --help`);
    Deno.exit(0);
  }
  // Build's unlabeled default is the latest channel (matching build:core /
  // build:client), unlike the release lib's stable default.
  return {
    channel: parseChannelFlag(args.channel ?? "latest"),
    force: args.force,
  };
}

/** Hash an item's compiled artifacts via its buildOutputs() path list. */
function outputHash(item: ReleaseItem, channel: ReleaseChannel): Promise<string> {
  return item.buildOutputs!(channel).then((paths) => hashPaths(paths.map((path) => ({ path }))));
}

function buildables(
  channel: ReleaseChannel,
  androidEnv: DeployEnv | null,
  iosEnv: DeployEnv | null,
): Buildable[] {
  return [
    {
      key: `core:${channel}`,
      label: "core + helpers",
      sourceHash: () => coreItem.sourceHash(channel),
      outputHash: () => outputHash(coreItem, channel),
      // In-process rather than the build-core.ts subprocess, so the build emits
      // the same artifact set a CI build runner does: binaries, helpers AND the
      // speech sidecar (build-core.ts runs only buildAll + buildHelpers).
      run: () => buildCoreArtifacts([detectHostTriple()], channelBinSuffix(channel)).then(() => {}),
    },
    {
      key: `client:${channel}`,
      label: "desktop client",
      sourceHash: () => clientItem.sourceHash(channel),
      outputHash: () => outputHash(clientItem, channel),
      cmd: ["run", "-A", "scripts/build-client.ts", `--channel=${channel}`],
    },
    {
      key: "catalog",
      label: "model catalog",
      sourceHash: () => catalogItem.sourceHash(channel),
      outputHash: () => outputHash(catalogItem, channel),
      cmd: ["run", "-A", "scripts/catalog/build.ts"],
    },
    // Android is host-independent (one APK set for every ABI). Only present when
    // a keystore is
    // configured; buildAndroidBundle keystore-signs into dist/android/, the same
    // path the release apply uses, so the output hash is stable across both.
    ...(androidEnv
      ? [
          {
            key: `android:${channel}`,
            label: "android client",
            sourceHash: () => androidItem.sourceHash(channel),
            outputHash: () => outputHash(androidItem, channel),
            run: () => buildAndroidBundle(androidEnv, channel).then(() => {}),
          },
        ]
      : []),
    // iOS mirrors android: only present when Apple signing is configured and the
    // mac toolchain exists. buildIosBundle signs into dist/ios/ (the same path
    // the release apply uses) and never uploads; the App Store Connect upload is
    // release-only.
    ...(iosEnv
      ? [
          {
            key: `ios:${channel}`,
            label: "ios client",
            sourceHash: () => iosItem.sourceHash(channel),
            outputHash: () => outputHash(iosItem, channel),
            run: () => buildIosBundle(iosEnv, channel).then(() => {}),
          },
        ]
      : []),
  ];
}

/** Whether a release keystore is configured in .env, checked without seeding
 *  keys so a plain desktop build stays side-effect-free. */
async function androidKeystoreConfigured(): Promise<boolean> {
  if (!(await exists(ENV_PATH))) return false;
  const raw = await loadDotenv({ envPath: ENV_PATH, export: false });
  return !!raw["LOCA_ANDROID_KEYSTORE_B64"];
}

/** The DeployEnv for the android item, or null (with a note) to skip it. Android
 *  builds only when BOTH the release keystore is configured AND the SDK/NDK/JDK
 *  toolchain is present, so a machine set up for desktop-only builds is never
 *  forced through a doomed gradle run. */
async function loadAndroidEnv(): Promise<DeployEnv | null> {
  if (!(await androidKeystoreConfigured())) {
    info(colors.yellow("Android keystore not set in .env; skipping the android client."));
    return null;
  }
  if (!androidToolchainReady()) {
    info(colors.yellow("Android SDK/NDK/JDK not found; skipping the android client."));
    return null;
  }
  return await loadOrSeedEnv();
}

/** Whether an Apple signing identity is configured in .env, checked without
 *  seeding keys (same reasoning as androidKeystoreConfigured). Signing, not the
 *  App Store Connect trio, is the bar: build-only produces a signed .ipa without
 *  uploading, and an unsigned build would emit no .ipa at all. */
async function iosSigningConfigured(): Promise<boolean> {
  if (!(await exists(ENV_PATH))) return false;
  const raw = await loadDotenv({ envPath: ENV_PATH, export: false });
  return !!raw["APPLE_TEAM_ID"] && (!!raw["APPLE_SIGNING_IDENTITY"] || !!raw["APPLE_CERTIFICATE"]);
}

/** The DeployEnv for the ios item, or null (with a note) to skip it. iOS builds
 *  only when BOTH Apple signing is configured AND this is a mac with Xcode + the
 *  ios rust target, so a machine set up for desktop-only builds is never forced
 *  through a doomed xcodebuild run. */
async function loadIosEnv(): Promise<DeployEnv | null> {
  if (!(await iosSigningConfigured())) {
    info(colors.yellow("Apple signing not set in .env; skipping the ios client."));
    return null;
  }
  if (!iosToolchainReady()) {
    info(
      colors.yellow("macOS with Xcode + the ios rust target not found; skipping the ios client."),
    );
    return null;
  }
  return await loadOrSeedEnv();
}

async function readState(): Promise<BuildState> {
  if (!(await exists(BUILD_STATE))) return {};
  try {
    return JSON.parse(await Deno.readTextFile(BUILD_STATE)) as BuildState;
  } catch {
    return {};
  }
}

async function writeState(state: BuildState): Promise<void> {
  await ensureDir(DIST_DIR);
  await Deno.writeTextFile(BUILD_STATE, JSON.stringify(state, null, 2) + "\n");
}

/** Why an item must build, or "" if it's up to date (source and output both
 *  match the cursor). The output hash is only computed on the up-to-date
 *  candidate path, so the rebuild cases stay cheap. */
async function buildReason(
  item: Buildable,
  rec: BuildRecord | undefined,
  source: string,
  force: boolean,
): Promise<string> {
  if (force) return "forced";
  if (!rec) return "never built";
  if (rec.source !== source) return "source changed";
  if ((await item.outputHash()) !== rec.output) {
    return "outputs missing or changed";
  }
  return "";
}

async function runBuildable(b: Buildable): Promise<void> {
  if (b.run) return await b.run();
  const cmd = b.cmd!;
  const p = new Deno.Command("deno", {
    args: cmd,
    cwd: REPO_ROOT,
    stdout: "inherit",
    stderr: "inherit",
  });
  const { code } = await p.output();
  if (code !== 0) fail(`build exited ${code}: deno ${cmd.join(" ")}`);
}

async function main(): Promise<void> {
  const flags = parseFlags();
  console.log(colors.bold(`\nloca build: ${flags.channel} channel (${detectHostTriple()})\n`));

  // Android is built only when a keystore is configured AND its toolchain is
  // present; loadAndroidEnv notes the reason and returns null to skip otherwise,
  // so a machine without android setup still builds the desktop items.
  const androidEnv = await loadAndroidEnv();
  // Same for iOS: Apple signing configured AND a mac with Xcode, else a note.
  const iosEnv = await loadIosEnv();

  const state = await readState();
  const items = buildables(flags.channel, androidEnv, iosEnv);
  let built = 0;
  for (const item of items) {
    const source = await item.sourceHash();
    const reason = await buildReason(item, state[item.key], source, flags.force);
    if (!reason) {
      info(`up to date: ${item.label}`);
      continue;
    }
    step(`Building: ${item.label} (${reason})`);
    await runBuildable(item);
    // Record source + output hashes only after a successful build, and persist
    // immediately so a later failure doesn't force the just-built items to
    // rebuild on the next run.
    state[item.key] = { source, output: await item.outputHash() };
    await writeState(state);
    built++;
  }

  if (built === 0) ok("everything up to date; nothing to build");
  else ok(`built ${built}/${items.length} item(s) (${flags.channel})`);
}

if (import.meta.main) {
  try {
    await main();
  } catch (err) {
    fail(err instanceof Error ? err.message : String(err));
  }
}
