// Native installer building blocks + the Linux Core packages + the installers
// manifest publisher.
//
// The user-facing Windows .exe and macOS .pkg are the UNIFIED installers
// (client + optional core) built in unified-installers.ts; this module owns
// what they share - payload assembly, signing/notarization helpers, the
// record/publish plumbing - plus the standalone Linux Core packages
// (.deb/.rpm), the one platform that keeps a separate client (AppImage) and
// core download.
//
// Every package delegates provisioning to the core binary's own install
// subcommands (packages/tomat-core/src/install): the post-install hook runs
// `tomat-core install-service` (admin token, built-in extension plant, launchd
// / systemd-user / Scheduled Task) and removal runs `tomat-core
// uninstall-service`. Because the runtime state lives per-user under
// ~/.tomat/<channel>/core, the post-install hooks resolve the target user and
// install into their home (a root pkg / deb postinstall targeting a
// user-domain service).
//
// VALIDATION NOTE: this module invokes productsign/notarytool, signtool,
// dpkg-deb, and rpmbuild - it only runs on a runner provisioned with those
// tools (the CI matrix + the local drivers install them). It cannot be
// exercised in a plain dev checkout; the templates + argument wiring are
// correct-by-construction and verified on a real release run.

import { basename, join } from "@std/path";
import { ensureDir } from "@std/fs/ensure-dir";
import type { Triple } from "../../packages/tomat-shared/src/domain/model.ts";
import type { CoreBuildArtifacts } from "./core.ts";
import { reanchorFile } from "./artifacts.ts";
import {
  type ApplyOpts,
  channelBinSuffix,
  channelManifestDir,
  channelStoragePrefix,
  colors,
  type DeployEnv,
  DIST_DIR,
  fetchLiveJson,
  humanBytes,
  info,
  ok,
  mapPool,
  R2_CONCURRENCY,
  r2Copy,
  r2Put,
  type ReleaseChannel,
  rel,
  sha256File,
  signEd25519Bytes,
  step,
} from "./lib.ts";

// ---------------------------------------------------------------------------
// types

/** One built native installer, copied under dist/<triple>/ so it rides the same
 *  publish path as every other artifact. `kind` separates the unified
 *  client+core installers (Windows .exe, macOS .pkg) from the core-only Linux
 *  packages; `version` is the namespace the asset uploads under (the client
 *  version for unified installers, the core version for core packages). */
export interface InstallerAsset {
  kind: "unified" | "core";
  format: "pkg" | "exe" | "deb" | "rpm";
  triple: Triple;
  version: string;
  filename: string;
  relPath: string; // dist-relative, e.g. "aarch64-apple-darwin/tomat-0.1.5.pkg"
  sha256: string;
  size: number;
}

export interface CoreInstallerOpts {
  version: string;
  channel: ReleaseChannel;
  triple: Triple;
  env: DeployEnv;
}

/** Version-less download filename for a Linux Core package, used for the
 *  `current/` alias the website links against without knowing the version. Kept
 *  in sync with the linuxCoreInstallers() helper in the website's
 *  lib/install.ts. */
export function coreInstallerStableName(
  format: InstallerAsset["format"],
  channel: ReleaseChannel,
): string {
  const suffix = channelBinSuffix(channel);
  return `tomat-core${suffix}.${format}`;
}

// ---------------------------------------------------------------------------
// small process helper (no engine host / logger; runs on a release machine)

export async function run(argv: string[], cwd?: string): Promise<void> {
  const { code, stderr } = await new Deno.Command(argv[0], {
    args: argv.slice(1),
    cwd,
    stdout: "inherit",
    stderr: "piped",
  }).output();
  if (code !== 0) {
    throw new Error(
      `${argv[0]} exited ${code}: ${new TextDecoder().decode(stderr).trim().slice(0, 4000)}`,
    );
  }
}

export async function have(bin: string): Promise<boolean> {
  // Presence, not success: Deno.Command throws NotFound when the binary is not on
  // PATH but returns a (possibly non-zero) exit code when it exists. Some tools we
  // probe reject `--version` (pkgbuild wants an argument, makensis uses /VERSION),
  // so keying off exit code 0 would false-negative and silently skip the installer.
  // stdin is closed so a tool that would otherwise read a script/stdin can't hang.
  try {
    await new Deno.Command(bin, {
      args: ["--version"],
      stdin: "null",
      stdout: "null",
      stderr: "null",
    }).output();
    return true;
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// payload assembly (shared across all four packagers)

/** Stage the core binary + matching helpers + workers into `<root>/{bin,workers}`
 *  using the channel-suffixed on-disk names the runtime expects. Returns the
 *  installed binary's basename (e.g. tomat-core-latest[.exe]). Shared by the
 *  Linux packages here and the unified installers (unified-installers.ts). */
export async function assemblePayload(
  built: CoreBuildArtifacts,
  triple: Triple,
  channel: ReleaseChannel,
  root: string,
): Promise<{ binName: string; binRelDir: string }> {
  const suffix = channelBinSuffix(channel);
  const exe = triple.endsWith("pc-windows-msvc") ? ".exe" : "";
  const binDir = join(root, "bin");
  const workersDir = join(root, "workers");
  await ensureDir(binDir);
  await ensureDir(workersDir);

  const core = built.artifacts.find((a) => a.triple === triple);
  if (!core) throw new Error(`no core binary for ${triple}`);
  const binName = `tomat-core${suffix}${exe}`;
  await Deno.copyFile(core.path, join(binDir, binName));

  for (const h of built.helpers) {
    if (h.triple !== triple) continue;
    // h.filename already carries the channel suffix + platform exe.
    await Deno.copyFile(h.path, join(binDir, h.filename));
  }
  for (const w of built.workers) {
    await Deno.copyFile(w.path, join(workersDir, basename(w.path)));
  }
  return { binName, binRelDir: "bin" };
}

// ---------------------------------------------------------------------------
// public entry (Linux)

/** Build the standalone Linux Core packages (.deb + .rpm). Returns the assets
 *  produced, or fewer/none when a packaging tool is unavailable (so a plain
 *  host build degrades rather than fails). The Windows and macOS user-facing
 *  installers are the unified ones (unified-installers.ts). */
export function buildLinuxCoreInstallers(
  built: CoreBuildArtifacts,
  opts: CoreInstallerOpts,
): Promise<InstallerAsset[]> {
  return buildLinuxPackages(built, opts);
}

/** Copy a produced installer under dist/<triple>/ and describe it. */
export async function record(
  builtPath: string,
  triple: Triple,
  kind: InstallerAsset["kind"],
  format: InstallerAsset["format"],
  version: string,
): Promise<InstallerAsset> {
  const filename = basename(builtPath);
  const relPath = `${triple}/${filename}`;
  await ensureDir(join(DIST_DIR, triple));
  const dest = join(DIST_DIR, relPath);
  await Deno.copyFile(builtPath, dest);
  const { sha256, size } = await sha256File(dest);
  ok(`  installer: ${filename}  ${humanBytes(size)}`);
  return { kind, format, triple, version, filename, relPath, sha256, size };
}

/** Notarize + staple a signed .pkg when notarization creds exist; otherwise warn
 *  (Gatekeeper will still block a signed-but-unnotarized pkg on download). */
export async function notarizePkg(pkg: string, env: DeployEnv): Promise<void> {
  const args = ["notarytool", "submit", pkg, "--wait"];
  if (env.appleApiKey && env.appleApiIssuer && env.appleApiKeyPath) {
    args.push(
      "--key",
      env.appleApiKeyPath,
      "--key-id",
      env.appleApiKey,
      "--issuer",
      env.appleApiIssuer,
    );
  } else if (env.appleId && env.applePassword && env.appleTeamId) {
    args.push(
      "--apple-id",
      env.appleId,
      "--password",
      env.applePassword,
      "--team-id",
      env.appleTeamId,
    );
  } else {
    info(colors.yellow(`no notarization credentials; signed but NOT notarized`));
    return;
  }
  await run(["xcrun", ...args]);
  await run(["xcrun", "stapler", "staple", pkg]);
}

// ---------------------------------------------------------------------------
// Windows Authenticode (dormant until a cert is configured)

export async function signWindows(file: string, env: DeployEnv): Promise<void> {
  if (env.windowsSignCommand) {
    // A user-provided sign command with %1 replaced by the file (Tauri's convention).
    const argv = env.windowsSignCommand.replace(/%1/g, file).split(/\s+/);
    await run(argv);
    return;
  }
  await run([
    "signtool",
    "sign",
    "/sha1",
    env.windowsCertificateThumbprint,
    "/fd",
    "sha256",
    "/tr",
    env.windowsTimestampUrl,
    "/td",
    "sha256",
    file,
  ]);
}

// ---------------------------------------------------------------------------
// Linux .deb + .rpm (dpkg-deb / rpmbuild)

async function buildLinuxPackages(
  built: CoreBuildArtifacts,
  opts: CoreInstallerOpts,
): Promise<InstallerAsset[]> {
  const assets: InstallerAsset[] = [];
  const deb = await buildDeb(built, opts).catch((e) => {
    info(colors.yellow(`Core .deb build skipped: ${e instanceof Error ? e.message : e}`));
    return null;
  });
  if (deb) assets.push(deb);
  const rpm = await buildRpm(built, opts).catch((e) => {
    info(colors.yellow(`Core .rpm build skipped: ${e instanceof Error ? e.message : e}`));
    return null;
  });
  if (rpm) assets.push(rpm);
  return assets;
}

function debArch(triple: Triple): string {
  return triple.startsWith("aarch64") ? "arm64" : "amd64";
}
function rpmArch(triple: Triple): string {
  return triple.startsWith("aarch64") ? "aarch64" : "x86_64";
}

async function buildDeb(
  built: CoreBuildArtifacts,
  opts: CoreInstallerOpts,
): Promise<InstallerAsset> {
  if (!(await have("dpkg-deb"))) throw new Error("dpkg-deb not available");
  const { version, channel, triple } = opts;
  const suffix = channelBinSuffix(channel);
  const pkgName = `tomat-core${suffix}`;
  const work = await Deno.makeTempDir({ prefix: "tomat-core-deb-" });
  const debRoot = join(work, pkgName);
  const prefix = `/usr/lib/${pkgName}`;
  const { binName } = await assemblePayload(built, triple, channel, join(debRoot, prefix.slice(1)));

  const debian = join(debRoot, "DEBIAN");
  await ensureDir(debian);
  await Deno.writeTextFile(
    join(debian, "control"),
    `Package: ${pkgName}\nVersion: ${version}\nArchitecture: ${debArch(triple)}\n` +
      `Maintainer: Tomat <noreply@au.tomat.ing>\nSection: utils\nPriority: optional\n` +
      `Description: Tomat Core - the local-first AI client service\n`,
  );
  const postinst = join(debian, "postinst");
  const prerm = join(debian, "prerm");
  const postrm = join(debian, "postrm");
  await Deno.writeTextFile(postinst, linuxPostinst(channel, prefix, binName));
  await Deno.writeTextFile(prerm, linuxPrerm(channel, prefix, binName));
  await Deno.writeTextFile(postrm, linuxPostrm(channel));
  await Deno.chmod(postinst, 0o755);
  await Deno.chmod(prerm, 0o755);
  await Deno.chmod(postrm, 0o755);

  const outFile = join(work, `${pkgName}_${version}_${debArch(triple)}.deb`);
  // zstd instead of the default xz: xz spends ~2 min on the multi-hundred-MB core
  // binary while zstd takes seconds, for a ~20% larger .deb. The level is pinned:
  // dpkg's own zstd default is 3, which costs another ~20% of size for a tenth of
  // a second, and 19 is slower than the xz it replaces. zstd needs dpkg 1.21.18+
  // (Debian 12+, Ubuntu 18.04+); on anything older dpkg-deb rejects -Zzstd, so
  // fall back to xz rather than let buildLinuxPackages swallow the failure and
  // publish a release with no .deb at all.
  const buildWith = (compression: string[]) =>
    run(["dpkg-deb", "--root-owner-group", ...compression, "--build", debRoot, outFile]);
  await buildWith(["-Zzstd", "-z9"]).catch(async (e) => {
    info(colors.yellow(`dpkg-deb -Zzstd failed (${e instanceof Error ? e.message : e}); using xz`));
    await buildWith(["-Zxz"]);
  });
  return await record(outFile, triple, "core", "deb", version);
}

async function buildRpm(
  built: CoreBuildArtifacts,
  opts: CoreInstallerOpts,
): Promise<InstallerAsset> {
  if (!(await have("rpmbuild"))) throw new Error("rpmbuild not available");
  const { version, channel, triple } = opts;
  const suffix = channelBinSuffix(channel);
  const pkgName = `tomat-core${suffix}`;
  const work = await Deno.makeTempDir({ prefix: "tomat-core-rpm-" });
  const prefix = `/usr/lib/${pkgName}`;
  const buildroot = join(work, "buildroot");
  const { binName } = await assemblePayload(
    built,
    triple,
    channel,
    join(buildroot, prefix.slice(1)),
  );

  // The deno-compiled core embeds its whole payload as a ~0.5 GB `.note.sui` ELF
  // note. rpm classifies every packaged file through libmagic, whose note-size cap
  // rejects that note ("Note section size too big"), failing the build; no spec
  // macro disables that classification. dpkg-deb has no such step, so only rpm
  // needs this: ship the core binary gzip-compressed (libmagic sees gzip data, not
  // the ELF) and decompress it in %post. The small helper ELFs classify fine, so
  // they stay raw. -n keeps the archive reproducible (no name/mtime in the header).
  await run(["gzip", "-n", join(buildroot, prefix.slice(1), "bin", binName)]);

  for (const d of ["SPECS", "RPMS", "BUILD", "BUILDROOT"]) await ensureDir(join(work, d));
  const spec = join(work, "SPECS", `${pkgName}.spec`);
  await Deno.writeTextFile(
    spec,
    // Disable rpm's default post-install processing: brp-strip runs `strip` over
    // the payload (harmless on the raw helper ELFs, but pointless), and
    // debug_package nil suppresses the debuginfo subpackage. The core binary is
    // shipped gzipped (see above) so brp never touches it either way.
    `%global __os_install_post %{nil}\n%global debug_package %{nil}\n\n` +
      `Name: ${pkgName}\nVersion: ${version}\nRelease: 1\nSummary: Tomat Core service\n` +
      `License: proprietary\nBuildArch: ${rpmArch(triple)}\n\n` +
      `%description\ntomat Core - the local-first AI client service.\n\n` +
      `%files\n${prefix}\n\n` +
      `%post\n${linuxPostBody(channel, prefix, binName, { gunzipCore: true })}\n\n` +
      `%preun\n${linuxPreunBody(channel, prefix, binName)}\n`,
  );
  await run([
    "rpmbuild",
    "-bb",
    "--define",
    `_topdir ${work}`,
    "--define",
    `_rpmdir ${work}/RPMS`,
    "--buildroot",
    buildroot,
    spec,
  ]);
  // rpmbuild writes RPMS/<arch>/<name>-<ver>-1.<arch>.rpm.
  const rpmDir = join(work, "RPMS", rpmArch(triple));
  let outFile = "";
  for await (const e of Deno.readDir(rpmDir)) {
    if (e.name.endsWith(".rpm")) outFile = join(rpmDir, e.name);
  }
  if (!outFile) throw new Error("rpmbuild produced no .rpm");
  return await record(outFile, triple, "core", "rpm", version);
}

// The install/removal bodies are shared between the deb maintainer scripts and
// the rpm scriptlets. They resolve the invoking user (packages install as root),
// copy the payload into that user's ~/.tomat/<channel>/core, and run the
// binary's own service subcommands as that user.
function linuxPostBody(
  channel: ReleaseChannel,
  prefix: string,
  binName: string,
  opts: { gunzipCore?: boolean } = {},
): string {
  // rpm ships the core binary gzipped (its ELF note breaks rpm's file classifier -
  // see buildRpm); decompress the copy in the user's home so install-service finds
  // the runnable binary. deb ships it raw and skips this.
  const gunzip = opts.gunzipCore ? `\n  gunzip -f "$DEST/bin/${binName}.gz"` : "";
  return `TARGET_USER="\${SUDO_USER:-$(logname 2>/dev/null || true)}"
if [ -z "$TARGET_USER" ] || [ "$TARGET_USER" = "root" ]; then
  echo "no target user; skipping per-user service registration" >&2
else
  USER_HOME="$(getent passwd "$TARGET_USER" | cut -d: -f6)"
  DEST="$USER_HOME/.tomat/${channel}/core"
  mkdir -p "$DEST/bin" "$DEST/workers"
  cp -R "${prefix}/bin/." "$DEST/bin/"${gunzip}
  cp -R "${prefix}/workers/." "$DEST/workers/"
  chown -R "$TARGET_USER" "$USER_HOME/.tomat/${channel}"
  # On a first-ever install the mkdir above created ~/.tomat itself as root; the
  # user's install-service must be able to create the shared models dir in it.
  chown "$TARGET_USER" "$USER_HOME/.tomat"
  su - "$TARGET_USER" -c "TOMAT_CHANNEL=${channel} '$DEST/bin/${binName}' install-service" || true
fi`;
}
export function linuxPreunBody(
  channel: ReleaseChannel,
  _prefix: string,
  binName: string,
  opts: { keepData?: boolean } = {},
): string {
  // Only tear down on a genuine removal, never on upgrade. dpkg runs prerm with
  // "remove" on removal and "upgrade" on upgrade; rpm runs %preun with 0 on the
  // final erase and 1 on upgrade. Running uninstall-service on upgrade would wipe
  // ~/.tomat/<channel>/core (sessions, memories, keychain master key) on every
  // version bump; on upgrade the new package's postinst re-runs the idempotent
  // install-service instead, which preserves that data.
  //
  // keepData maps dpkg's remove-vs-purge split onto uninstall-service: `apt
  // remove` (prerm "remove") only stops + unregisters the service, and the data
  // wipe waits for `dpkg -P` (postrm "purge", linuxPostrmBody). rpm has no purge
  // concept - its final erase IS the purge - so %preun keeps the full teardown.
  const flags = opts.keepData ? " --keep-data" : "";
  return `case "$1" in
remove|0)
  TARGET_USER="\${SUDO_USER:-$(logname 2>/dev/null || true)}"
  if [ -n "$TARGET_USER" ] && [ "$TARGET_USER" != "root" ]; then
    USER_HOME="$(getent passwd "$TARGET_USER" | cut -d: -f6)"
    DEST="$USER_HOME/.tomat/${channel}/core"
    su - "$TARGET_USER" -c "TOMAT_CHANNEL=${channel} '$DEST/bin/${binName}' uninstall-service${flags}" || true
  fi
  ;;
esac`;
}
export function linuxPostrmBody(channel: ReleaseChannel): string {
  // dpkg -P (purge) removes the data `apt remove` kept. The package payload -
  // the core binary included - is already gone by postrm, so this is a plain
  // rm -rf of the per-user channel core dir; the shared ~/.tomat/models dir is
  // always kept, and the keychain master key may linger (the binary that could
  // clear it is gone) - both documented in the manual's uninstalling article.
  return `case "$1" in
purge)
  TARGET_USER="\${SUDO_USER:-$(logname 2>/dev/null || true)}"
  if [ -n "$TARGET_USER" ] && [ "$TARGET_USER" != "root" ]; then
    USER_HOME="$(getent passwd "$TARGET_USER" | cut -d: -f6)"
    rm -rf "$USER_HOME/.tomat/${channel}/core"
    rmdir "$USER_HOME/.tomat/${channel}" 2>/dev/null || true
  fi
  ;;
esac`;
}
function linuxPostinst(channel: ReleaseChannel, prefix: string, binName: string): string {
  return `#!/bin/sh\nset -e\n${linuxPostBody(channel, prefix, binName)}\nexit 0\n`;
}
function linuxPrerm(channel: ReleaseChannel, prefix: string, binName: string): string {
  return `#!/bin/sh\nset -e\n${linuxPreunBody(channel, prefix, binName, { keepData: true })}\nexit 0\n`;
}
function linuxPostrm(channel: ReleaseChannel): string {
  return `#!/bin/sh\nset -e\n${linuxPostrmBody(channel)}\nexit 0\n`;
}

// ---------------------------------------------------------------------------
// publish: upload the installers + a signed installers.json manifest

export interface InstallersManifest {
  /** The core version the core-kind entries (and every unified payload's Core
   *  half) were built from. */
  coreVersion: string;
  /** The client version the unified installers wrap (and are uploaded under). */
  clientVersion: string;
  installers: Array<{
    kind: InstallerAsset["kind"];
    format: InstallerAsset["format"];
    triple: Triple;
    filename: string;
    url: string;
    sha256: string;
    size: number;
  }>;
}

/** Version-less current/ alias filename for an installer, the stable URL the
 *  website links without knowing versions. Kept in sync with the download
 *  helpers (appInstallers/linuxCoreInstallers) in the website's lib/install.ts. */
export function installerAliasName(a: InstallerAsset, channel: ReleaseChannel): string {
  if (a.kind === "core") return coreInstallerStableName(a.format, channel);
  const suffix = channelBinSuffix(channel);
  return a.format === "exe" ? `tomat${suffix}-setup.exe` : `tomat${suffix}.pkg`;
}

/** Upload every built installer (the unified Windows .exe / macOS .pkg and the
 *  Linux Core packages) to R2 and publish a signed installers.json (Ed25519,
 *  same trust root as core.json) the website's download CTA reads. Carries
 *  forward same-version entries from the live manifest a single-runner publish
 *  didn't rebuild - only while BOTH versions match, so a bump on either side
 *  drops the stale entries. Returns the installer binaries as GitHub-Release
 *  assets ({ path, flat name }) so the caller can mirror them to the rolling
 *  release exactly like client bundles/APKs; the installers.json + .sig it
 *  writes under dist/<manifestDir> are picked up by the publisher's manifest
 *  walk automatically. */
export async function uploadInstallers(
  env: DeployEnv,
  channel: ReleaseChannel,
  versions: { coreVersion: string; clientVersion?: string },
  installers: InstallerAsset[],
  opts: ApplyOpts,
): Promise<Array<{ path: string; name: string }>> {
  if (installers.length === 0) return [];
  const manifestDir = channelManifestDir(channel);
  const storagePrefix = channelStoragePrefix(channel);
  const live = await fetchLiveJson<InstallersManifest>(env, `${manifestDir}/installers.json`);

  // A run that staged no client (a Linux-only publish: deb/rpm carry the core
  // version alone) keeps the live manifest's client version so the unified
  // entries it carries forward stay truthful.
  const coreVersion = versions.coreVersion;
  const clientVersion = versions.clientVersion ?? live?.clientVersion ?? "";

  step(`Composing installers.json (${installers.length} installer(s))`);
  // Keep entries this run did not rebuild (keyed by triple+filename).
  const byKey = new Map<string, InstallersManifest["installers"][number]>();
  if (live?.coreVersion === coreVersion && live?.clientVersion === clientVersion) {
    for (const e of live.installers) byKey.set(`${e.triple}/${e.filename}`, e);
  }
  const uploads: Array<{
    key: string;
    path: string;
    label: string;
    size: number;
    aliasKey: string;
  }> = [];
  for (const a of installers) {
    const path = await reanchorFile(a.relPath, a.sha256);
    // Each asset uploads under its own version namespace (client version for
    // unified installers, core version for the Linux packages; see
    // InstallerAsset.version).
    const key = `${storagePrefix}${a.version}/${a.triple}/${a.filename}`;
    byKey.set(`${a.triple}/${a.filename}`, {
      kind: a.kind,
      format: a.format,
      triple: a.triple,
      filename: a.filename,
      url: `https://${env.storageDomain}/${key}`,
      sha256: a.sha256,
      size: a.size,
    });
    uploads.push({
      key,
      path,
      label: `${a.triple}_${a.filename}`,
      size: a.size,
      // Version-less alias so the website links a stable download URL without
      // knowing the version (mirrors android.ts's current/ APK alias).
      aliasKey: `${storagePrefix}current/${a.triple}/${installerAliasName(a, channel)}`,
    });
  }

  const manifest: InstallersManifest = {
    coreVersion,
    clientVersion,
    installers: [...byKey.values()],
  };

  const outDir = join(DIST_DIR, manifestDir);
  await ensureDir(outDir);
  const manifestPath = join(outDir, "installers.json");
  await Deno.writeTextFile(manifestPath, JSON.stringify(manifest, null, 2));
  ok(`installers.json → ${rel(manifestPath)}`);

  // Detached Ed25519 signature over the exact manifest bytes (same trust root +
  // shape as client.json.sig), so a consumer can authenticate it before trusting
  // any URL/sha256 it lists.
  const sig = await signEd25519Bytes(env.signingPrivateKey, await Deno.readFile(manifestPath));
  const sigPath = join(outDir, "installers.json.sig");
  await Deno.writeTextFile(sigPath, sig);

  if (opts.dryRun) {
    info(colors.yellow(`dry-run: skipping upload of ${uploads.length} installer(s) + manifest`));
    return [];
  }
  step(`Uploading installer(s) to R2 bucket "${env.r2Bucket}"`);
  const ghAssets = await mapPool(uploads, R2_CONCURRENCY, async (u) => {
    info(`uploading ${u.key}  (${humanBytes(u.size)})`);
    await r2Put(env, u.key, u.path, "application/octet-stream");
    opts.recordVersionedKey?.(u.key);
    // Mirror to the version-less current/ alias the website links against via a
    // server-side copy (no re-upload). Short cache (via the same header the
    // manifests use); the versioned copy above is the source of truth the signed
    // installers.json names.
    info(`copying ${u.aliasKey}  (alias)`);
    await r2Copy(env, u.key, u.aliasKey, u.path, "application/octet-stream", "public, max-age=300");
    // u.label is already the per-triple-unique flat name (`<triple>_<filename>`),
    // which is exactly what the GitHub-Release mirror needs to avoid basename
    // clobbering across triples.
    return { path: u.path, name: u.label };
  });
  await r2Put(
    env,
    `${manifestDir}/installers.json`,
    manifestPath,
    "application/json",
    "public, max-age=300",
  );
  await r2Put(
    env,
    `${manifestDir}/installers.json.sig`,
    sigPath,
    "text/plain",
    "public, max-age=300",
  );
  ok(`https://${env.storageDomain}/${manifestDir}/installers.json`);
  return ghAssets;
}
