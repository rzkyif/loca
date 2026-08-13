// Release item: the Android APK of the client, self-hosted on R2 with an
// Ed25519-signed android.json (the mobile analogue of client.json). Tauri's
// updater plugin has no Android support, so unlike the desktop client there is
// no per-bundle minisign signature: the Java keystore signs the APK for install
// (Android verifies it), and android.json's detached Ed25519 signature lets the
// in-app updater authenticate the manifest + sha256 before downloading.
//
// Designed so a future Google Play (AAB) track is an additive release item, not
// a rewrite: the applicationId is stable and this self-host path is independent
// of any Play upload.

import { ensureDir } from "@std/fs/ensure-dir";
import { walk } from "@std/fs/walk";
import { join } from "@std/path";
import { decodeBase64 } from "@std/encoding/base64";
import { type AndroidApkRecord, type AndroidDescriptor, reanchorFile } from "./artifacts.ts";
import {
  type ApplyOpts,
  bumpVersionField,
  channelManifestDir,
  channelStoragePrefix,
  colors,
  type DeployEnv,
  DIST_DIR,
  exists,
  fail,
  fetchLiveJson,
  hashPaths,
  humanBytes,
  info,
  ok,
  packagesHashInputs,
  mapPool,
  R2_CONCURRENCY,
  r2Copy,
  r2Put,
  rel,
  type ReleaseChannel,
  type ReleaseItem,
  REPO_ROOT,
  sha256File,
  signEd25519Bytes,
  step,
  stripJsonVersion,
} from "./lib.ts";

// ---------------------------------------------------------------------------
// paths

const TAURI_DIR = join(REPO_ROOT, "packages/tomat-client/src/tauri");
const TAURI_CONF_PATH = join(TAURI_DIR, "tauri.conf.json");
const GEN_ANDROID_DIR = join(TAURI_DIR, "gen/android");
const APK_OUT_DIR = join(GEN_ANDROID_DIR, "app/build/outputs/apk");
const KEYSTORE_JKS_PATH = join(GEN_ANDROID_DIR, "release.jks");
const KEYSTORE_PROPS_PATH = join(GEN_ANDROID_DIR, "keystore.properties");

const MANIFEST_CACHE_CONTROL = "public, max-age=300";

// ---------------------------------------------------------------------------
// types

interface AndroidApk {
  /** Manifest ABI key, e.g. "android-arm64". */
  abi: string;
  path: string;
  filename: string;
  size: number;
}

interface AndroidManifest {
  version: string;
  notes: string;
  pub_date: string;
  /** One entry per built ABI; the in-app updater picks its device's ABI. */
  abis: Record<string, { url: string; sha256: string }>;
}

// gradle ABI directory/file fragments -> the android.json key the updater reads.
const ABI_KEYS: Array<[fragment: string, key: string]> = [
  ["arm64-v8a", "android-arm64"],
  ["armeabi-v7a", "android-armv7"],
  ["x86_64", "android-x86_64"],
  ["x86", "android-x86"],
];

function abiKeyFor(filename: string): string {
  for (const [fragment, key] of ABI_KEYS) {
    if (filename.includes(fragment)) return key;
  }
  // A single fat APK (no per-ABI split) covers every device.
  return "android-universal";
}

/** Version is the one baked into the APK by Tauri (tauri.conf.json), exactly
 *  what the in-app updater compares android.json against. */
async function readAndroidVersion(): Promise<string> {
  const conf = JSON.parse(await Deno.readTextFile(TAURI_CONF_PATH)) as {
    version?: string;
  };
  if (!conf.version) fail(`no version in ${rel(TAURI_CONF_PATH)}`);
  return conf.version;
}

// ---------------------------------------------------------------------------
// keystore materialization

/** Decode the release keystore from the env into gen/android and write the
 *  keystore.properties the app's build.gradle.kts reads. Returns a cleanup that
 *  removes both secret files. Fails if the keystore env is not configured. */
async function materializeKeystore(env: DeployEnv): Promise<() => Promise<void>> {
  if (!env.androidKeystoreB64) {
    fail(
      `TOMAT_ANDROID_KEYSTORE_B64 is not set in .env; cannot sign the release ` +
        `APK. Generate a keystore with keytool and base64-encode it into .env ` +
        `(see DEVELOPMENT.md).`,
    );
  }
  await Deno.writeFile(KEYSTORE_JKS_PATH, decodeBase64(env.androidKeystoreB64));
  const props = [
    `storeFile=${KEYSTORE_JKS_PATH}`,
    `storePassword=${env.androidKeystorePassword}`,
    `keyAlias=${env.androidKeyAlias}`,
    `keyPassword=${env.androidKeyPassword}`,
    "",
  ].join("\n");
  await Deno.writeTextFile(KEYSTORE_PROPS_PATH, props);

  // Best-effort synchronous wipe if the process is interrupted before `finally`
  // runs, so the decoded signing keystore never lingers on disk after a Ctrl-C.
  const wipeSync = (): void => {
    try {
      Deno.removeSync(KEYSTORE_JKS_PATH);
    } catch {
      /* already gone */
    }
    try {
      Deno.removeSync(KEYSTORE_PROPS_PATH);
    } catch {
      /* already gone */
    }
  };
  const onSignal = (): void => {
    wipeSync();
    Deno.exit(130);
  };
  Deno.addSignalListener("SIGINT", onSignal);
  Deno.addSignalListener("SIGTERM", onSignal);

  return async () => {
    Deno.removeSignalListener("SIGINT", onSignal);
    Deno.removeSignalListener("SIGTERM", onSignal);
    await Deno.remove(KEYSTORE_JKS_PATH).catch(() => {});
    await Deno.remove(KEYSTORE_PROPS_PATH).catch(() => {});
  };
}

// ---------------------------------------------------------------------------
// build + locate APKs

async function buildAndroid(channel: ReleaseChannel): Promise<void> {
  const cmd = new Deno.Command("deno", {
    args: ["run", "-A", "scripts/build-android.ts", `--channel=${channel}`],
    cwd: REPO_ROOT,
    stdout: "inherit",
    stderr: "inherit",
    env: { ...Deno.env.toObject(), TOMAT_CHANNEL: channel },
  });
  const { code } = await cmd.output();
  if (code !== 0) fail(`build-android.ts (${channel}) exited ${code}`);
}

/** Every signed release APK under the gradle output tree, keyed by ABI. Skips
 *  unsigned intermediates so only the keystore-signed artifacts are published. */
async function findApks(): Promise<AndroidApk[]> {
  if (!(await exists(APK_OUT_DIR))) {
    fail(`no APK output dir at ${rel(APK_OUT_DIR)} (did the build run?)`);
  }
  const apks: AndroidApk[] = [];
  for await (const entry of walk(APK_OUT_DIR, { exts: [".apk"], includeDirs: false })) {
    if (!entry.path.includes("/release/")) continue;
    if (entry.name.includes("-unsigned")) continue;
    const stat = await Deno.stat(entry.path);
    apks.push({
      abi: abiKeyFor(entry.name),
      path: entry.path,
      filename: entry.name,
      size: stat.size,
    });
  }
  if (apks.length === 0) fail(`no signed release APK found under ${rel(APK_OUT_DIR)}`);
  return apks;
}

function composeAndroidManifest(
  version: string,
  abis: Record<string, { url: string; sha256: string }>,
): AndroidManifest {
  return {
    version,
    notes: `Tomat ${version}`,
    pub_date: new Date().toISOString(),
    abis,
  };
}

async function writeManifestFile(
  manifestDir: string,
  name: string,
  body: unknown,
): Promise<string> {
  const dir = join(DIST_DIR, manifestDir);
  await ensureDir(dir);
  const path = join(dir, name);
  await Deno.writeTextFile(path, JSON.stringify(body, null, 2));
  return path;
}

// ---------------------------------------------------------------------------
// release item

// The APK bundles the same Tauri shell + shared UI as the desktop client, so it
// shares the client's source-hash inputs (client + shared, src + manifest each).
const ANDROID_PACKAGES = ["client", "shared"];
const ANDROID_HASH_INPUTS = packagesHashInputs(ANDROID_PACKAGES);

export const androidItem: ReleaseItem = {
  id: "android",
  label: "android client",
  scope: "channel",
  packages: ANDROID_PACKAGES,
  bumpHint: "packages/tomat-client/src/tauri/tauri.conf.json (version)",

  version: readAndroidVersion,
  versionFile: TAURI_CONF_PATH,
  bumpVersion: () => bumpVersionField(TAURI_CONF_PATH),

  sourceHash(_channel: ReleaseChannel): Promise<string> {
    // tauri.conf.json carries the client/android version; drop it from the tree
    // hash and fold it back in with the version blanked, so a lone bump does not
    // re-trigger an android release while a real tauri.conf.json change still does.
    const confRel = rel(TAURI_CONF_PATH);
    return hashPaths([
      ...ANDROID_HASH_INPUTS.map((p) => ({
        path: join(REPO_ROOT, p),
        exclude: (r: string) =>
          r.endsWith(".test.ts") ||
          r.includes("/target/") ||
          r.includes("/gen/android/") ||
          r === confRel,
      })),
      { path: TAURI_CONF_PATH, transform: stripJsonVersion },
    ]);
  },

  // The keystore-signed APKs buildAndroidBundle() copies under dist/android/.
  // The unified build hash-checks this so a wiped/swapped output rebuilds.
  buildOutputs(_channel: ReleaseChannel): Promise<string[]> {
    return Promise.resolve([join(DIST_DIR, "android")]);
  },

  // Fill-only: the CURRENT version is published but carries no ABIs (a partial /
  // broken publish at this version), so republish to repair it - no version bump.
  // A published version merely being BEHIND the local (bumped) version is NOT a
  // change: a lone version bump must not rebuild/republish; it ships with the next
  // real source change, which trips the source hash above.
  async extraChanged(env: DeployEnv, channel: ReleaseChannel): Promise<boolean> {
    const version = await readAndroidVersion();
    const manifestDir = channelManifestDir(channel);
    const live = await fetchLiveJson<AndroidManifest>(env, `${manifestDir}/android.json`);
    return !!live && live.version === version && Object.keys(live.abis ?? {}).length === 0;
  },

  async apply(env: DeployEnv, channel: ReleaseChannel, opts: ApplyOpts): Promise<void> {
    const version = await readAndroidVersion();
    // Prebuilt mode (CI publish): the Linux build runner already keystore-signed
    // the APKs; just compose + upload. Otherwise build them here.
    if (opts.prebuilt && !opts.prebuilt.android) {
      info(colors.yellow("no android bundle produced; skipping android publish"));
      return;
    }
    const descriptor = opts.prebuilt?.android ?? (await buildAndroidBundle(env, channel));
    await composeAndUploadAndroid(env, channel, version, descriptor, opts);
  },
};

// ---------------------------------------------------------------------------
// build half (runs on the Linux build runner: local apply or ci-build.ts)

/** Build + keystore-sign the APK(s), copy each under dist/android/<abi>/ so they
 *  survive the move to the publish host, and return the descriptor the publish
 *  half composes android.json from. Needs the Android keystore (build-time
 *  signing) but NOT the Ed25519 manifest key (that signs android.json on the
 *  publish host), so a build runner never holds the trust-root private key. */
export async function buildAndroidBundle(
  env: DeployEnv,
  channel: ReleaseChannel,
): Promise<AndroidDescriptor> {
  const version = await readAndroidVersion();
  info(`android version ${version}`);

  step("Building signed Android APK");
  const cleanupKeystore = await materializeKeystore(env);
  try {
    await buildAndroid(channel);
    const apks = await findApks();
    const records: AndroidApkRecord[] = [];
    for (const a of apks) {
      ok(`${a.abi}/${a.filename}  ${humanBytes(a.size)}`);
      const relPath = `android/${a.abi}/${a.filename}`;
      await ensureDir(join(DIST_DIR, "android", a.abi));
      await Deno.copyFile(a.path, join(DIST_DIR, relPath));
      const { sha256 } = await sha256File(a.path);
      records.push({ abi: a.abi, filename: a.filename, relPath, sha256, size: a.size });
    }
    return { version, channel, apks: records };
  } finally {
    await cleanupKeystore();
  }
}

// ---------------------------------------------------------------------------
// publish half (runs ONCE on the host)

/** Compose + Ed25519-sign android.json from a built descriptor, upload the
 *  versioned APKs + the version-less `current/` alias + the manifest to R2. */
export async function composeAndUploadAndroid(
  env: DeployEnv,
  channel: ReleaseChannel,
  version: string,
  descriptor: AndroidDescriptor,
  opts: ApplyOpts,
): Promise<void> {
  const manifestDir = channelManifestDir(channel);
  const storagePrefix = channelStoragePrefix(channel);

  step("Composing android.json");
  const abis: Record<string, { url: string; sha256: string }> = {};
  const uploads: Array<{ abi: string; path: string; key: string; aliasKey: string; size: number }> =
    [];
  for (const a of descriptor.apks) {
    // Re-anchor + verify the APK bytes against the descriptor's sha256.
    const path = await reanchorFile(a.relPath, a.sha256);
    const key = `${storagePrefix}${version}/${a.abi}/tomat.apk`;
    abis[a.abi] = { url: `https://${env.storageDomain}/${key}`, sha256: a.sha256 };
    uploads.push({
      abi: a.abi,
      path,
      key,
      aliasKey: `${storagePrefix}current/${a.abi}/tomat.apk`,
      size: a.size,
    });
  }
  const manifest = composeAndroidManifest(version, abis);
  const androidJsonPath = await writeManifestFile(manifestDir, "android.json", manifest);
  ok(`android.json → ${rel(androidJsonPath)}`);

  // Detached Ed25519 signature over the exact android.json bytes, so the in-app
  // updater authenticates the manifest before downloading. Mirrors client.json.sig.
  const androidJsonSig = await signEd25519Bytes(
    env.signingPrivateKey,
    await Deno.readFile(androidJsonPath),
  );
  const androidSigPath = join(DIST_DIR, manifestDir, "android.json.sig");
  await Deno.writeTextFile(androidSigPath, androidJsonSig);
  ok(`android.json.sig → ${rel(androidSigPath)}`);

  if (opts.dryRun) {
    info(colors.yellow(`dry-run: skipping upload of APK(s) + ${manifestDir}/android.json`));
    return;
  }

  step(`Uploading APK(s) to R2 bucket "${env.r2Bucket}"`);
  await mapPool(uploads, R2_CONCURRENCY, async (u) => {
    info(`uploading ${u.key}  (${humanBytes(u.size)})`);
    await r2Put(env, u.key, u.path, "application/vnd.android.package-archive");
    opts.recordVersionedKey?.(u.key);
    opts.recordReleaseAsset?.(u.path, `android-${u.abi}_tomat.apk`);

    // Mirror each APK to a version-less "current" alias so the install page can
    // link a stable download URL without knowing the version, via a server-side
    // copy (no re-upload). Short cache; the versioned copy above stays the source
    // of truth the signed manifest names.
    info(`copying ${u.aliasKey}  (alias)`);
    await r2Copy(
      env,
      u.key,
      u.aliasKey,
      u.path,
      "application/vnd.android.package-archive",
      MANIFEST_CACHE_CONTROL,
    );
  });

  step(`Uploading ${manifestDir}/android.json to R2`);
  await r2Put(
    env,
    `${manifestDir}/android.json`,
    androidJsonPath,
    "application/json",
    MANIFEST_CACHE_CONTROL,
  );
  ok(`https://${env.storageDomain}/${manifestDir}/android.json`);

  step(`Uploading ${manifestDir}/android.json.sig to R2`);
  await r2Put(
    env,
    `${manifestDir}/android.json.sig`,
    androidSigPath,
    "text/plain",
    MANIFEST_CACHE_CONTROL,
  );
  ok(`https://${env.storageDomain}/${manifestDir}/android.json.sig`);
}
