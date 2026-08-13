// Release item: the iOS build of the client, distributed through the App Store
// (TestFlight + App Store Connect). Unlike Android, iOS cannot sideload a signed
// .ipa, so there is no R2 self-host, no ios.json, and no Ed25519 manifest: Apple
// signs the app and the store owns updates (mobile.ts's updater.check returns
// null on iOS). The upload therefore happens from the signing mac itself (App
// Store Connect API), not from the Linux publish coordinator that composes the
// R2 manifests.
//
// The whole item is secret-driven: main.ts drops it (with a yellow warning)
// whenever appleReleaseConfigured(env) is false, exactly as it drops the desktop
// client without Tauri keys and android without a keystore. Filling the APPLE_*
// env is the only switch. See scripts/release/macos-signing.md.

import { ensureDir } from "@std/fs/ensure-dir";
import { walk } from "@std/fs/walk";
import { join } from "@std/path";
import { appleReleaseConfigured, iosToolchainReady, resolveAppleEnv } from "./apple-toolchain.ts";
import {
  type ApplyOpts,
  bumpVersionField,
  colors,
  type DeployEnv,
  DIST_DIR,
  exists,
  fail,
  hashPaths,
  humanBytes,
  info,
  ok,
  packagesHashInputs,
  rel,
  type ReleaseChannel,
  type ReleaseItem,
  REPO_ROOT,
  sha256File,
  step,
  stripJsonVersion,
} from "./lib.ts";

// ---------------------------------------------------------------------------
// paths

const TAURI_DIR = join(REPO_ROOT, "packages/tomat-client/src/tauri");
const TAURI_CONF_PATH = join(TAURI_DIR, "tauri.conf.json");
const GEN_APPLE_DIR = join(TAURI_DIR, "gen/apple");

/** Version is the one baked into the app by Tauri (tauri.conf.json), shared with
 *  the desktop + android clients (they all bump this file once). */
async function readIosVersion(): Promise<string> {
  const conf = JSON.parse(await Deno.readTextFile(TAURI_CONF_PATH)) as { version?: string };
  if (!conf.version) fail(`no version in ${rel(TAURI_CONF_PATH)}`);
  return conf.version;
}

// ---------------------------------------------------------------------------
// build + locate the .ipa

async function buildIos(env: DeployEnv, channel: ReleaseChannel): Promise<void> {
  const cmd = new Deno.Command("deno", {
    args: ["run", "-A", "scripts/build-ios.ts", `--channel=${channel}`],
    cwd: REPO_ROOT,
    stdout: "inherit",
    stderr: "inherit",
    // resolveAppleEnv matters locally: the APPLE_* values live in .env (loaded
    // with export: false), so without it build-ios.ts would see no APPLE_TEAM_ID
    // and silently fall back to an unsigned Simulator build.
    env: { ...Deno.env.toObject(), TOMAT_CHANNEL: channel, ...resolveAppleEnv(env) },
  });
  const { code } = await cmd.output();
  if (code !== 0) fail(`build-ios.ts (${channel}) exited ${code}`);
}

/** The signed .ipa Tauri exported under gen/apple. */
async function findIpa(): Promise<{ path: string; filename: string; size: number }> {
  if (!(await exists(GEN_APPLE_DIR))) {
    fail(
      `no gen/apple dir at ${rel(GEN_APPLE_DIR)} (run \`deno task --cwd packages/tomat-client init:ios\` first)`,
    );
  }
  for await (const entry of walk(GEN_APPLE_DIR, { exts: [".ipa"], includeDirs: false })) {
    const stat = await Deno.stat(entry.path);
    return { path: entry.path, filename: entry.name, size: stat.size };
  }
  fail(`no .ipa found under ${rel(GEN_APPLE_DIR)} (did the signed build run?)`);
}

// ---------------------------------------------------------------------------
// App Store Connect upload
//
// Uploads a signed .ipa via `xcrun altool` using the App Store Connect API key
// (APPLE_API_KEY / APPLE_API_ISSUER). The app record on App Store Connect must
// exist BEFORE the first upload (created manually in the ASC UI); the App
// Review submission after TestFlight processing is also manual. See
// macos-signing.md.
//
// DEPRECATION WATCH: Apple has deprecated `altool --upload-app` and may
// discontinue it. If an upload starts failing with a deprecation/unsupported
// error, switch to `xcrun iTMSTransporter -m upload` (ships with Xcode) or an
// xcodebuild -exportArchive export with an `upload` destination. Verify the
// upload lands in App Store Connect -> TestFlight on the first real release.

async function uploadToAppStoreConnect(env: DeployEnv, ipaPath: string): Promise<void> {
  step("Uploading .ipa to App Store Connect");
  // altool does NOT read APPLE_API_KEY_PATH (that is a notarytool/Tauri
  // convention): it discovers the .p8 by directory convention (./private_keys,
  // ~/.appstoreconnect/private_keys, ...) or via API_PRIVATE_KEYS_DIR, and only
  // under the canonical AuthKey_<keyid>.p8 name. Stage a canonical copy in a
  // temp dir so the upload works wherever (and however named) the key lives.
  const keysRoot = await Deno.makeTempDir({ prefix: "tomat-altool-key-" });
  const keysDir = join(keysRoot, "private_keys");
  await ensureDir(keysDir);
  const p8Copy = join(keysDir, `AuthKey_${env.appleApiKey}.p8`);
  await Deno.copyFile(env.appleApiKeyPath, p8Copy);
  await Deno.chmod(p8Copy, 0o600);
  try {
    const cmd = new Deno.Command("xcrun", {
      args: [
        "altool",
        "--upload-app",
        "-f",
        ipaPath,
        "-t",
        "ios",
        "--apiKey",
        env.appleApiKey,
        "--apiIssuer",
        env.appleApiIssuer,
      ],
      env: { ...Deno.env.toObject(), ...resolveAppleEnv(env), API_PRIVATE_KEYS_DIR: keysDir },
      stdout: "inherit",
      stderr: "inherit",
    });
    const { code } = await cmd.output();
    if (code !== 0) fail(`xcrun altool upload exited ${code}`);
    ok("uploaded to App Store Connect (TestFlight processing follows)");
  } finally {
    await Deno.remove(keysRoot, { recursive: true }).catch(() => {});
  }
}

// ---------------------------------------------------------------------------
// build + upload (runs on the signing mac: local apply or ci-build.ts --ios)

/** Build the signed .ipa and copy it under dist/ios/ for the record. Needs the
 *  Apple signing env; also the build half of `deno task build`'s ios leg (which
 *  never uploads). Returns the dist-relative path of the copied .ipa. */
export async function buildIosBundle(env: DeployEnv, channel: ReleaseChannel): Promise<string> {
  const version = await readIosVersion();
  info(`ios version ${version}`);

  step("Building signed iOS .ipa");
  await buildIos(env, channel);
  const ipa = await findIpa();
  const relPath = `ios/${ipa.filename}`;
  await ensureDir(join(DIST_DIR, "ios"));
  await Deno.copyFile(ipa.path, join(DIST_DIR, relPath));
  const { sha256 } = await sha256File(ipa.path);
  ok(`${ipa.filename}  ${humanBytes(ipa.size)}  ${sha256.slice(0, 12)}…`);
  // Deliberately NOT mirrored to the GitHub Release: an .ipa there is not
  // installable (iOS installs come from the App Store), so publishing one would
  // mislead. The dist/ios copy above is kept only as a local build record.
  return relPath;
}

/** Build the signed .ipa, copy it under dist/ios/ for the record, and upload it
 *  to App Store Connect. Needs the full Apple signing + API env; the caller has
 *  already checked appleReleaseConfigured. */
export async function buildAndUploadIos(
  env: DeployEnv,
  channel: ReleaseChannel,
  opts: ApplyOpts,
): Promise<void> {
  const relPath = await buildIosBundle(env, channel);

  if (opts.dryRun) {
    info(colors.yellow("dry-run: skipping App Store Connect upload"));
    return;
  }
  await uploadToAppStoreConnect(env, join(DIST_DIR, relPath));
}

// ---------------------------------------------------------------------------
// release item

// The .ipa bundles the same Tauri shell + shared UI as the desktop + android
// clients, so it shares the client's source-hash inputs (client + shared).
const IOS_PACKAGES = ["client", "shared"];
const IOS_HASH_INPUTS = packagesHashInputs(IOS_PACKAGES);

export const iosItem: ReleaseItem = {
  id: "ios",
  label: "ios client",
  scope: "channel",
  packages: IOS_PACKAGES,
  bumpHint: "packages/tomat-client/src/tauri/tauri.conf.json (version)",

  version: readIosVersion,
  versionFile: TAURI_CONF_PATH,
  bumpVersion: () => bumpVersionField(TAURI_CONF_PATH),

  sourceHash(_channel: ReleaseChannel): Promise<string> {
    // tauri.conf.json carries the client/ios version; drop it from the tree hash
    // and fold it back in with the version blanked, so a lone bump does not
    // re-trigger an ios release while a real tauri.conf.json change still does.
    const confRel = rel(TAURI_CONF_PATH);
    return hashPaths([
      ...IOS_HASH_INPUTS.map((p) => ({
        path: join(REPO_ROOT, p),
        exclude: (r: string) =>
          r.endsWith(".test.ts") ||
          r.includes("/target/") ||
          r.includes("/gen/apple/") ||
          r === confRel,
      })),
      { path: TAURI_CONF_PATH, transform: stripJsonVersion },
    ]);
  },

  buildOutputs(_channel: ReleaseChannel): Promise<string[]> {
    return Promise.resolve([join(DIST_DIR, "ios")]);
  },

  async apply(env: DeployEnv, channel: ReleaseChannel, _opts: ApplyOpts): Promise<void> {
    // Defensive: main.ts already drops the item when signing/upload isn't
    // configured, so this only guards a direct call.
    if (!appleReleaseConfigured(env)) {
      info(colors.yellow("Apple signing / App Store Connect not configured; skipping iOS."));
      return;
    }
    if (!iosToolchainReady()) {
      info(colors.yellow("iOS release requires macOS with Xcode; skipping on this host."));
      return;
    }
    await buildAndUploadIos(env, channel, _opts);
  },
};
