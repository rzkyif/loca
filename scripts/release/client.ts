// Release item: the Tauri client bundle for the host platform, signed with the
// Tauri updater key, uploaded to R2, and merged into client.json. Tauri can't
// cross-compile across OSes for signing reasons, so this is host-only: each
// platform publishes its own bundle and the merge preserves the others at the
// same version. Versioned via packages/tomat-client/src/tauri/tauri.conf.json.

import { ensureDir } from "@std/fs/ensure-dir";
import { encodeBase64 } from "@std/encoding/base64";
import { join } from "@std/path";
import type { Triple } from "../../packages/tomat-shared/src/domain/model.ts";
import { type ClientDescriptor, reanchorFile } from "./artifacts.ts";
import {
  buildLinuxCoreInstallers,
  type InstallerAsset,
  type InstallersManifest,
  uploadInstallers,
} from "./core-installers.ts";
import {
  buildMacUnified,
  buildWindowsUnified,
  materializeCorePayload,
} from "./unified-installers.ts";
import { reportRouting, routeTriples } from "./all-targets.ts";
import { withEnvironment } from "./drivers/lifecycle.ts";
import type { BuildEnvironment } from "./drivers/mod.ts";
import {
  type ApplyOpts,
  bumpVersionField,
  channelBinSuffix,
  channelManifestDir,
  channelStoragePrefix,
  colors,
  type DeployEnv,
  detectHostTriple,
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
const TAURI_BUNDLE_OUT = join(REPO_ROOT, "target/release/bundle");
// Legacy placeholder, kept for forks that still ship it.
const TAURI_PUBKEY_PLACEHOLDER = "PLACEHOLDER_REPLACE_AT_BUILD_TIME";

const MANIFEST_CACHE_CONTROL = "public, max-age=300";

// ---------------------------------------------------------------------------
// types

interface ClientBundle {
  triple: Triple;
  bundlePath: string;
  sigPath: string;
  filename: string;
  size: number;
}

interface ClientManifest {
  version: string;
  notes: string;
  pub_date: string;
  platforms: Record<string, { signature: string; url: string; sha256: string }>;
}

/** The client's published version is the one baked into the app bundle by Tauri
 *  (tauri.conf.json), which is exactly what Tauri's updater compares against
 *  client.json. Sourcing it from anywhere else risks an update loop. */
async function readClientVersion(): Promise<string> {
  const conf = JSON.parse(await Deno.readTextFile(TAURI_CONF_PATH)) as {
    version?: string;
  };
  if (!conf.version) fail(`no version in ${rel(TAURI_CONF_PATH)}`);
  return conf.version;
}

// ---------------------------------------------------------------------------
// tauri platform key (the release scripts use the Rust triple; tauri's
// updater consumes <os>-<arch> instead)

function tauriPlatformKey(triple: Triple): string {
  if (triple.endsWith("apple-darwin")) {
    return triple.startsWith("aarch64") ? "darwin-aarch64" : "darwin-x86_64";
  }
  if (triple.endsWith("pc-windows-msvc")) {
    return triple.startsWith("aarch64") ? "windows-aarch64" : "windows-x86_64";
  }
  if (triple.endsWith("unknown-linux-gnu")) {
    return triple.startsWith("aarch64") ? "linux-aarch64" : "linux-x86_64";
  }
  fail(`no tauri platform key for triple ${triple}`);
}

// ---------------------------------------------------------------------------
// tauri.conf.json pubkey reconciliation

async function injectTauriPubkey(pubkey: string): Promise<() => Promise<void>> {
  const original = await Deno.readTextFile(TAURI_CONF_PATH);
  const committed = JSON.parse(original)?.plugins?.updater?.pubkey;
  if (committed === pubkey) return async () => {};
  if (committed === TAURI_PUBKEY_PLACEHOLDER) {
    const patched = original.replace(TAURI_PUBKEY_PLACEHOLDER, pubkey);
    await Deno.writeTextFile(TAURI_CONF_PATH, patched);
    ok(`substituted tauri updater pubkey placeholder in tauri.conf.json`);
    return async () => {
      await Deno.writeTextFile(TAURI_CONF_PATH, original);
      info(`restored tauri.conf.json placeholder`);
    };
  }
  fail(
    `tauri.conf.json plugins.updater.pubkey does not match .env's ` +
      `TAURI_UPDATER_PUBLIC_KEY. Either commit the right pubkey or clear ` +
      `the field back to "${TAURI_PUBKEY_PLACEHOLDER}".`,
  );
}

// ---------------------------------------------------------------------------
// build client + locate bundle

async function buildClient(
  env: DeployEnv,
  channel: ReleaseChannel,
  triple?: Triple,
  bundles?: string[],
): Promise<void> {
  // Drive build-client.ts directly with the channel so it bakes TOMAT_CHANNEL
  // + applies the per-channel app-identity config override. `triple` cross-builds
  // the host's other arch; `bundles` narrows the bundle targets (the Linux
  // cross-build passes just "appimage" - the AppImage is the sole Linux client
  // format).
  const args = ["run", "-A", "scripts/build-client.ts", `--channel=${channel}`];
  if (triple) args.push(`--target=${triple}`);
  if (bundles?.length) args.push(`--bundles=${bundles.join(",")}`);
  // CI passes every APPLE_* secret through the build step, so the ones that do
  // not exist arrive as empty strings. Tauri treats a present-but-empty
  // APPLE_CERTIFICATE as "sign me" and fails `security import`. Deno.Command
  // inherits the parent env (clearEnv defaults false), so dropping the key from
  // the env object below is not enough - the blank still leaks in from this
  // process. Delete the blanks from the process env itself so no descendant
  // (build-client.ts, then Tauri) inherits them; appleSigningEnv re-adds only the
  // ones actually set, keeping the inert-by-default ad-hoc signing.
  for (const [key, value] of Object.entries(Deno.env.toObject())) {
    if (key.startsWith("APPLE_") && value === "") Deno.env.delete(key);
  }
  const cmd = new Deno.Command("deno", {
    args,
    cwd: REPO_ROOT,
    stdout: "inherit",
    stderr: "inherit",
    env: {
      ...Deno.env.toObject(),
      TAURI_SIGNING_PRIVATE_KEY: env.tauriUpdaterPrivateKey,
      TAURI_SIGNING_PRIVATE_KEY_PASSWORD: env.tauriUpdaterPassword,
      ...appleSigningEnv(env, triple ?? detectHostTriple()),
    },
  });
  const { code } = await cmd.output();
  if (code !== 0) fail(`build-client.ts (${channel}) exited ${code}`);
}

/** The Apple Developer ID signing + notarization env vars Tauri reads, but only
 *  for a macOS target and only for the fields that are actually set. When the
 *  .env has no Apple credentials (the default), this returns {} and the macOS
 *  build keeps ad-hoc signing (signingIdentity "-") with no notarization - i.e.
 *  the current behavior. Empty values are dropped so Tauri never sees a blank
 *  APPLE_CERTIFICATE it would try (and fail) to import. Exported for the
 *  inert-by-default regression test (client.test.ts). */
export function appleSigningEnv(env: DeployEnv, triple: Triple): Record<string, string> {
  if (!triple.endsWith("apple-darwin")) return {};
  const candidates: Record<string, string> = {
    APPLE_SIGNING_IDENTITY: env.appleSigningIdentity,
    APPLE_CERTIFICATE: env.appleCertificateB64,
    APPLE_CERTIFICATE_PASSWORD: env.appleCertificatePassword,
    APPLE_ID: env.appleId,
    APPLE_PASSWORD: env.applePassword,
    APPLE_TEAM_ID: env.appleTeamId,
    APPLE_API_KEY: env.appleApiKey,
    APPLE_API_ISSUER: env.appleApiIssuer,
    APPLE_API_KEY_PATH: env.appleApiKeyPath,
  };
  return Object.fromEntries(Object.entries(candidates).filter(([, v]) => v !== ""));
}

/** Locate the built installer + its Tauri `.sig` for `triple` under `bundleRoot`
 *  - `target/release/bundle` for a host-native build, or
 *  `target/<triple>/release/bundle` for a cross-targeted one. The Linux client is
 *  a self-contained AppImage (the GUI deps are bundled into it); like every other
 *  platform it carries the Tauri updater `.sig`. */
async function findClientBundle(triple: Triple, bundleRoot: string): Promise<ClientBundle> {
  const candidates: { dir: string; ext: string }[] = [];
  if (triple.endsWith("apple-darwin")) {
    candidates.push({ dir: join(bundleRoot, "macos"), ext: ".app.tar.gz" });
  } else if (triple.endsWith("pc-windows-msvc")) {
    // Windows ships the per-user NSIS installer (installMode currentUser in
    // tauri.conf.json); the MSI target was dropped so first install needs no
    // admin. Prefer nsis; keep msi as a fallback only so a stale bundle dir from
    // an older build is still locatable rather than silently shipped.
    candidates.push({ dir: join(bundleRoot, "nsis"), ext: ".exe" });
    candidates.push({ dir: join(bundleRoot, "msi"), ext: ".msi" });
  } else if (triple.endsWith("unknown-linux-gnu")) {
    candidates.push({ dir: join(bundleRoot, "appimage"), ext: ".AppImage" });
  }
  for (const c of candidates) {
    if (!(await exists(c.dir))) continue;
    for await (const entry of Deno.readDir(c.dir)) {
      if (!entry.isFile) continue;
      if (!entry.name.endsWith(c.ext)) continue;
      const sigPath = join(c.dir, `${entry.name}.sig`);
      if (!(await exists(sigPath))) continue;
      const bundlePath = join(c.dir, entry.name);
      const stat = await Deno.stat(bundlePath);
      return { triple, bundlePath, sigPath, filename: entry.name, size: stat.size };
    }
  }
  fail(
    `no Tauri bundle + .sig found for ${triple} under ` +
      `${rel(bundleRoot)} (checked ${candidates.map((c) => `${rel(c.dir)}/*${c.ext}`).join(", ")})`,
  );
}

/** Dormant Windows Authenticode signing. When a cert is configured (env, via
 *  DeployEnv), patch tauri.conf.json's bundle.windows so Tauri signs the NSIS
 *  installer, restoring the file afterward (mirrors injectTauriPubkey). When the
 *  cert env is empty - the default, no cert yet - this is a no-op and the
 *  installer ships unsigned (the install flow keeps stripping Mark-of-the-Web).
 *  Only applies to a windows target. Tauri reads Windows signing from config, not
 *  env, which is why this is a config patch rather than an env injection like
 *  appleSigningEnv. */
export async function injectWindowsSigning(
  env: DeployEnv,
  triple: Triple,
): Promise<() => Promise<void>> {
  if (!triple.endsWith("pc-windows-msvc")) return async () => {};
  if (!env.windowsCertificateThumbprint && !env.windowsSignCommand) return async () => {};
  const original = await Deno.readTextFile(TAURI_CONF_PATH);
  const conf = JSON.parse(original);
  conf.bundle ??= {};
  conf.bundle.windows ??= {};
  if (env.windowsCertificateThumbprint) {
    conf.bundle.windows.certificateThumbprint = env.windowsCertificateThumbprint;
  }
  if (env.windowsSignCommand) conf.bundle.windows.signCommand = env.windowsSignCommand;
  conf.bundle.windows.timestampUrl = env.windowsTimestampUrl;
  await Deno.writeTextFile(TAURI_CONF_PATH, JSON.stringify(conf, null, 2));
  ok(`enabled Windows Authenticode signing in tauri.conf.json`);
  return async () => {
    await Deno.writeTextFile(TAURI_CONF_PATH, original);
    info(`restored tauri.conf.json (removed Windows signing)`);
  };
}

// The primary installable a plain `deno task build:client` emits for the host
// platform (no updater .tar.gz/.sig, which a keyless build skips). The unified
// build hashes these so a wiped or swapped bundle forces a rebuild. macOS
// returns the .app directory (hashPaths walks it); others the installer file.
export async function clientBuildOutputs(): Promise<string[]> {
  const triple = detectHostTriple();
  const checks: { dir: string; ext: string }[] = [];
  if (triple.endsWith("apple-darwin")) {
    checks.push({ dir: join(TAURI_BUNDLE_OUT, "macos"), ext: ".app" });
  } else if (triple.endsWith("pc-windows-msvc")) {
    checks.push({ dir: join(TAURI_BUNDLE_OUT, "msi"), ext: ".msi" });
    checks.push({ dir: join(TAURI_BUNDLE_OUT, "nsis"), ext: ".exe" });
  } else if (triple.endsWith("unknown-linux-gnu")) {
    checks.push({ dir: join(TAURI_BUNDLE_OUT, "appimage"), ext: ".AppImage" });
  }
  const found: string[] = [];
  for (const c of checks) {
    if (!(await exists(c.dir))) continue;
    for await (const entry of Deno.readDir(c.dir)) {
      if (entry.name.endsWith(c.ext)) found.push(join(c.dir, entry.name));
    }
  }
  return found;
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
// build half (runs on each native platform: local apply or a CI build runner)

/** Build + Tauri-sign the host platform's client bundle, copy it (and its .sig)
 *  under dist/<triple>/ so it survives the move to the publish host, and return
 *  the descriptor the publish half composes client.json from. Used by the local
 *  apply (then composed immediately) and by scripts/release/ci-build.ts (staged
 *  for the coordinator). The Ed25519 manifest key is NOT needed here, only the
 *  Tauri minisign key, so a build runner never holds the trust-root private key. */
export async function buildClientBundle(
  env: DeployEnv,
  channel: ReleaseChannel,
  opts: { triple?: Triple; bundles?: string[] } = {},
): Promise<ClientDescriptor> {
  const version = await readClientVersion();
  const triple = opts.triple ?? detectHostTriple();
  const tauriKey = tauriPlatformKey(triple);
  // A host-native build (no --target) emits under target/release/bundle; a
  // cross-targeted one under <CARGO_TARGET_DIR|target>/<triple>/release/bundle
  // (the Podman client build points CARGO_TARGET_DIR at /tmp so it doesn't write
  // the read-only repo mount).
  const targetDir = Deno.env.get("CARGO_TARGET_DIR") ?? join(REPO_ROOT, "target");
  const bundleRoot = opts.triple ? join(targetDir, triple, "release", "bundle") : TAURI_BUNDLE_OUT;
  info(`client triple: ${triple} (tauri key: ${tauriKey}), version ${version}`);

  step(`Building Tauri client bundle (${triple})`);
  const restorePubkey = await injectTauriPubkey(env.tauriUpdaterPublicKey);
  const restoreWinSigning = await injectWindowsSigning(env, triple);
  try {
    await buildClient(env, channel, opts.triple, opts.bundles);
    const bundle = await findClientBundle(triple, bundleRoot);
    ok(`${triple}/${bundle.filename}  ${humanBytes(bundle.size)}  → ${rel(bundle.sigPath)}`);

    // Copy the bundle + its Tauri .sig under dist/<triple>/ so they ride along
    // with the core artifacts; the publish host re-anchors + verifies.
    const relPath = `${triple}/${bundle.filename}`;
    const sigRelPath = `${triple}/${bundle.filename}.sig`;
    await ensureDir(join(DIST_DIR, triple));
    await Deno.copyFile(bundle.bundlePath, join(DIST_DIR, relPath));
    await Deno.copyFile(bundle.sigPath, join(DIST_DIR, sigRelPath));

    // sha256 over the exact bytes the updater downloads (mirrors core.json):
    // the Tauri minisign signature protects in-app updates, this protects the
    // transit between build runner and publish host (reanchorFile).
    const signature = (await Deno.readTextFile(bundle.sigPath)).trim();
    const { sha256 } = await sha256File(bundle.bundlePath);
    return {
      version,
      channel,
      triple,
      tauriKey,
      filename: bundle.filename,
      relPath,
      sigRelPath,
      sha256,
      size: bundle.size,
      signature,
    };
  } finally {
    await restoreWinSigning();
    await restorePubkey();
  }
}

// ---------------------------------------------------------------------------
// publish half (runs ONCE on the host over every platform's descriptor)

/** Version-less current/ alias for a platform's PRIMARY client bundle, when
 *  that bundle is itself the user-facing download: only the Linux AppImage (the
 *  one platform without a unified installer). The Windows and macOS downloads
 *  are the unified installers (their aliases are written by uploadInstallers),
 *  and macOS's primary bundle is the `.app.tar.gz` updater payload, not a user
 *  download. Kept in sync with the website's lib/install.ts. */
function clientBundleAlias(
  triple: Triple,
  storagePrefix: string,
  suffix: string,
): string | undefined {
  if (triple.endsWith("unknown-linux-gnu")) {
    return `${storagePrefix}current/${triple}/tomat${suffix}.AppImage`;
  }
  return undefined;
}

/** A non-stable channel's productName carries the channel-suffixed DISPLAY name
 *  ("Tomat (latest)"), which Tauri bakes verbatim - spaces and parens included -
 *  into the built bundle filename. That filename becomes the R2 object key AND
 *  the `url` in client.json the updater fetches, so a raw space/paren would land
 *  in the CDN URL and break the download. Strip the " (channel)" wrapper back to
 *  a URL-safe "-channel" here (mirrors android.ts keying by a fixed clean name).
 *  Stable's productName has no such wrapper, so this is a no-op there. The
 *  capitalized brand rides through: these versioned keys are only ever read back
 *  from client.json, never hand-typed. The version-less `current/` aliases the
 *  website links (clientBundleAlias, unifiedInstallerStableName) keep their own
 *  fixed lowercase names. Paired with the productName format in
 *  scripts/build-client.ts. */
function urlSafeBundleName(filename: string): string {
  return filename.replace(/ \(([^)]+)\)/, "-$1");
}

/** Compose client.json from the union of platform descriptors, Ed25519-sign the
 *  detached client.json.sig, and upload the bundles + manifest to R2. Carry
 *  forward any platform from the live manifest at the same version that this run
 *  did not (re)build, so a single-host local release still fills its own slot
 *  without dropping the others. */
export async function composeAndUploadClient(
  env: DeployEnv,
  channel: ReleaseChannel,
  version: string,
  descriptors: ClientDescriptor[],
  opts: ApplyOpts,
): Promise<void> {
  if (descriptors.length === 0) fail(`no client bundles to publish`);
  const manifestDir = channelManifestDir(channel);
  const storagePrefix = channelStoragePrefix(channel);
  const live = await fetchLiveJson<ClientManifest>(env, `${manifestDir}/client.json`);

  step("Composing client.json (Tauri updater manifest)");
  const platforms: ClientManifest["platforms"] =
    live?.version === version ? { ...live.platforms } : {};
  const suffix = channelBinSuffix(channel);
  const uploads: Array<{
    key: string;
    path: string;
    label: string;
    size: number;
    // Version-less current/ alias the website links against (mirrors android.ts).
    aliasKey?: string;
  }> = [];
  for (const d of descriptors) {
    // Re-anchor + verify the bundle bytes against the descriptor's sha256.
    const bundlePath = await reanchorFile(d.relPath, d.sha256);
    const key = `${storagePrefix}${version}/${d.triple}/${urlSafeBundleName(d.filename)}`;
    platforms[d.tauriKey] = {
      signature: d.signature,
      url: `https://${env.storageDomain}/${key}`,
      sha256: d.sha256,
    };
    uploads.push({
      key,
      path: bundlePath,
      label: `${d.triple}_${d.filename}`,
      size: d.size,
      aliasKey: clientBundleAlias(d.triple, storagePrefix, suffix),
    });
  }

  const manifest: ClientManifest = {
    version,
    notes: `Tomat ${version}`,
    pub_date: new Date().toISOString(),
    platforms,
  };
  const clientJsonPath = await writeManifestFile(manifestDir, "client.json", manifest);
  ok(`client.json → ${rel(clientJsonPath)}`);

  // Detached Ed25519 signature over the exact client.json bytes, so a consumer
  // can authenticate the manifest (and thus the sha256 it trusts) before use.
  const clientJsonSig = await signEd25519Bytes(
    env.signingPrivateKey,
    await Deno.readFile(clientJsonPath),
  );
  const clientSigPath = join(DIST_DIR, manifestDir, "client.json.sig");
  await Deno.writeTextFile(clientSigPath, clientJsonSig);
  ok(`client.json.sig → ${rel(clientSigPath)}`);

  if (opts.dryRun) {
    info(
      colors.yellow(`dry-run: skipping upload of client bundle(s) + ${manifestDir}/client.json`),
    );
    return;
  }

  step(`Uploading client bundle(s) to R2 bucket "${env.r2Bucket}"`);
  await mapPool(uploads, R2_CONCURRENCY, async (u) => {
    info(`uploading ${u.key}  (${humanBytes(u.size)})`);
    await r2Put(env, u.key, u.path, "application/octet-stream");
    opts.recordVersionedKey?.(u.key);
    opts.recordReleaseAsset?.(u.path, u.label);
    if (u.aliasKey) {
      info(`copying ${u.aliasKey}  (alias)`);
      await r2Copy(
        env,
        u.key,
        u.aliasKey,
        u.path,
        "application/octet-stream",
        "public, max-age=300",
      );
    }
  });

  step(`Uploading ${manifestDir}/client.json to R2`);
  await r2Put(
    env,
    `${manifestDir}/client.json`,
    clientJsonPath,
    "application/json",
    MANIFEST_CACHE_CONTROL,
  );
  ok(`https://${env.storageDomain}/${manifestDir}/client.json`);

  step(`Uploading ${manifestDir}/client.json.sig to R2`);
  await r2Put(
    env,
    `${manifestDir}/client.json.sig`,
    clientSigPath,
    "text/plain",
    MANIFEST_CACHE_CONTROL,
  );
  ok(`https://${env.storageDomain}/${manifestDir}/client.json.sig`);
}

// ---------------------------------------------------------------------------
// local all-targets build (host cross-arch + on-demand driver environments)

/** Build the client installer for every requested triple across this host + the
 *  given build environments, mirroring core's buildCoreUnified: the host builds
 *  its own OS's triples directly (native + same-OS cross-arch), and every other
 *  triple is routed to an environment (Podman/UTM) started on demand. Returns a
 *  descriptor per built triple for composeAndUploadClient. Triples whose driver
 *  can't build the client (or isn't available) are reported and dropped - the
 *  live manifest carries them forward at the same version. */
export async function buildClientUnified(
  triples: Triple[],
  env: DeployEnv,
  channel: ReleaseChannel,
  environments: BuildEnvironment[],
): Promise<ClientDescriptor[]> {
  const routing = await routeTriples(triples, environments);
  reportRouting(routing);

  const descriptors: ClientDescriptor[] = [];
  // Host: build each same-OS triple directly (e.g. both apple-darwin arches).
  for (const triple of routing.host) {
    descriptors.push(await buildClientBundle(env, channel, { triple }));
  }
  // Drivers: each environment builds its triples' installers and ships the
  // bundles + descriptors back into the host's dist/. Every desktop installer
  // (Windows MSI/NSIS, Linux AppImage) carries the Tauri updater `.sig`, so the
  // minisign key transits to each build environment.
  for (const { env: drv, triples: envTriples } of routing.byEnv) {
    if (!drv.buildClient) {
      info(
        colors.yellow(`${drv.id} can't build the client; ${envTriples.join(", ")} carry forward`),
      );
      continue;
    }
    const ds = await withEnvironment(drv, () =>
      drv.buildClient!({
        triples: envTriples,
        channel,
        secrets: {
          signingPublicKeyB64: encodeBase64(env.signingPublicKey),
          tauriPublicKey: env.tauriUpdaterPublicKey,
          tauriPrivateKey: env.tauriUpdaterPrivateKey,
          tauriPassword: env.tauriUpdaterPassword,
        },
      }),
    );
    descriptors.push(...ds);
  }
  return descriptors;
}

// ---------------------------------------------------------------------------
// release item

// The desktop client bundles the Tauri shell + the shared UI; its source hash
// is derived from those two packages (src + manifest each).
const CLIENT_PACKAGES = ["client", "shared"];
const CLIENT_HASH_INPUTS = packagesHashInputs(CLIENT_PACKAGES);

export const clientItem: ReleaseItem = {
  id: "client",
  label: "desktop client + installers",
  scope: "channel",
  packages: CLIENT_PACKAGES,
  bumpHint: "packages/tomat-client/src/tauri/tauri.conf.json (version)",

  version: readClientVersion,
  versionFile: TAURI_CONF_PATH,
  bumpVersion: () => bumpVersionField(TAURI_CONF_PATH),

  sourceHash(_channel: ReleaseChannel): Promise<string> {
    // tauri.conf.json carries the client version; drop it from the tree hash and
    // fold it back in with the version blanked, so a lone bump does not re-trigger
    // a client release while a real tauri.conf.json change still does.
    const confRel = rel(TAURI_CONF_PATH);
    return hashPaths([
      ...CLIENT_HASH_INPUTS.map((p) => ({
        path: join(REPO_ROOT, p),
        exclude: (r: string) => r.endsWith(".test.ts") || r.includes("/target/") || r === confRel,
      })),
      { path: TAURI_CONF_PATH, transform: stripJsonVersion },
    ]);
  },

  buildOutputs(_channel: ReleaseChannel): Promise<string[]> {
    return clientBuildOutputs();
  },

  // Two republish triggers beyond the source hash, both no-bump:
  // - Platform fill: the CURRENT version is already published but this host's
  //   platform is missing from it (another machine released a different
  //   platform). A published version merely being BEHIND the local (bumped)
  //   version is NOT a change: a lone version bump must not rebuild/republish;
  //   it ships with the next real source change, which trips the source hash.
  // - Stale installers: the live installers.json does not match the LIVE
  //   client.json + core.json versions (e.g. a core-only release just moved
  //   core.json), so the published unified installers wrap an outdated half and
  //   must be rebuilt. Live-vs-live on purpose: every client publish refreshes
  //   installers.json in the same run, so comparing against the LOCAL version
  //   (which ci-bump moves ahead right after a release) would flag "stale" on
  //   every subsequent run and publish a new client version per push, forever.
  //   Costs a client rebuild at the same version; acceptable.
  async extraChanged(env: DeployEnv, channel: ReleaseChannel): Promise<boolean> {
    const version = await readClientVersion();
    const manifestDir = channelManifestDir(channel);
    const live = await fetchLiveJson<ClientManifest>(env, `${manifestDir}/client.json`);
    const hostKey = tauriPlatformKey(detectHostTriple());
    if (!!live && live.version === version && !live.platforms[hostKey]) return true;
    const liveCore = await fetchLiveJson<{ version: string }>(env, `${manifestDir}/core.json`);
    if (!live || !liveCore) return false; // nothing published yet; source hash owns the first run
    const liveInstallers = await fetchLiveJson<InstallersManifest>(
      env,
      `${manifestDir}/installers.json`,
    );
    return (
      !liveInstallers ||
      liveInstallers.clientVersion !== live.version ||
      liveInstallers.coreVersion !== liveCore.version
    );
  },

  async apply(env: DeployEnv, channel: ReleaseChannel, opts: ApplyOpts): Promise<void> {
    const version = await readClientVersion();
    // Prebuilt mode (CI publish): compose over the descriptors the desktop build
    // runners produced. All-targets mode: build this host's OS triples + route
    // the rest to on-demand driver environments. Host-only mode: just this host's
    // bundle (the live manifest carries forward the other platforms at version).
    const descriptors = opts.prebuilt
      ? opts.prebuilt.clientDescriptors
      : opts.environments?.length
        ? await buildClientUnified(opts.triples, env, channel, opts.environments)
        : [await buildClientBundle(env, channel)];
    if (descriptors.length === 0) {
      info(colors.yellow("no client bundles produced; skipping client publish"));
      return;
    }
    await composeAndUploadClient(env, channel, version, descriptors, opts);
    // Native installers for a LOCAL release: package this host OS's installers
    // (the unified pkg/exe wrapping the bundle just built, or the Linux Core
    // deb/rpm) and publish installers.json. In CI each build runner packages
    // its own OS in ci-build.ts and the coordinator publishes (opts.prebuilt),
    // so this path is local-only. Cross-OS installers in local all-targets mode
    // inherit the host-OS-only gap: only this host's OS gets its installers;
    // the CI matrix covers the full set.
    if (!opts.prebuilt) {
      await buildAndUploadHostInstallers(env, channel, version, descriptors, opts);
    }
  },
};

/** Build + publish the HOST OS's native installers from a local release run:
 *  macOS/Windows wrap the host descriptor's just-built client bundle around the
 *  Core payload materialized from the live core.json (fresh when the core item
 *  published earlier in this run); Linux packages the Core alone (its client
 *  stays the AppImage). Skips with a note when the host bundle is missing or
 *  the packaging tools are absent. */
async function buildAndUploadHostInstallers(
  env: DeployEnv,
  channel: ReleaseChannel,
  clientVersion: string,
  descriptors: ClientDescriptor[],
  opts: ApplyOpts,
): Promise<void> {
  const hostTriple = detectHostTriple();
  const hostDescriptor = descriptors.find((d) => d.triple === hostTriple);
  if (!hostDescriptor) {
    info(colors.yellow("no host client bundle in this run; skipping the native installers"));
    return;
  }
  const { version: coreVersion, built } = await materializeCorePayload(env, channel, hostTriple);
  const unifiedOpts = { coreVersion, clientVersion, channel, triple: hostTriple, env };
  const installers: InstallerAsset[] = [];
  if (hostTriple.endsWith("apple-darwin")) {
    installers.push(...(await buildMacUnified(built, unifiedOpts)));
  } else if (hostTriple.endsWith("pc-windows-msvc")) {
    installers.push(...(await buildWindowsUnified(built, hostDescriptor.relPath, unifiedOpts)));
  } else if (hostTriple.endsWith("unknown-linux-gnu")) {
    installers.push(
      ...(await buildLinuxCoreInstallers(built, {
        version: coreVersion,
        channel,
        triple: hostTriple,
        env,
      })),
    );
  }
  if (installers.length > 0) {
    await uploadInstallers(env, channel, { coreVersion, clientVersion }, installers, opts);
  }
}
