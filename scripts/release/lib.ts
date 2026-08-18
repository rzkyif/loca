// Shared helpers for the release task family.
//
// Anything that touches .env, paths, signing, R2 uploads, or fetches lives
// here. Sub-scripts (core.ts, client.ts, schemas.ts, website.ts) compose these
// helpers; ci-publish.ts imports each sub-script's ReleaseItem to run the
// umbrella release.

import { encodeBase64 } from "@std/encoding/base64";
import { encodeHex } from "@std/encoding/hex";
import { copy } from "@std/fs/copy";
import { ensureDir } from "@std/fs/ensure-dir";
import { dirname, fromFileUrl, join, resolve } from "@std/path";
import { load as loadDotenv } from "@std/dotenv";
import * as ed from "@noble/ed25519";
import { AwsClient } from "aws4fetch";
import { canonicalize } from "../../packages/loca-shared/src/crypto/canonical.ts";
// Type-only: artifacts.ts imports values from this module, so a value import
// here would be a runtime cycle. `import type` is erased, so it is safe.
import type { PrebuiltStaging } from "./artifacts.ts";
// github-release.ts imports values back from this module; the ES-module cycle
// resolves because neither side calls the other at module-eval time.
import { publishGithubRelease } from "./github-release.ts";

// ---------------------------------------------------------------------------
// paths

export const REPO_ROOT = resolve(dirname(fromFileUrl(import.meta.url)), "../..");
export const WEBSITE_DIR = join(REPO_ROOT, "packages/loca-website");
export const CORE_DIR = join(REPO_ROOT, "packages/loca-core");
export const SHARED_DIR = join(REPO_ROOT, "packages/loca-shared");
export const DIST_DIR = join(REPO_ROOT, "dist");
export const ENV_PATH = join(REPO_ROOT, ".env");
export const ENV_EXAMPLE_PATH = join(REPO_ROOT, ".env.example");
export const SIGNING_KEYS_PATH = join(CORE_DIR, "data/signing-keys.json");
export const CONFIG_PATH = join(CORE_DIR, "src/config.ts");
/** The SHIPPED core's version constant.
 *
 *  `CONFIG_PATH` belongs to the TypeScript core, which is retained only as the
 *  conformance oracle and ships nowhere. Since the build cutover the artifact
 *  is built from `loca-core-rs`, so the version a running core REPORTS comes
 *  from here, and this is the one the release manifest must agree with. */
export const CORE_CONFIG_RS = join(REPO_ROOT, "packages/loca-core-rs/src/config.rs");

// ---------------------------------------------------------------------------
// pretty output

export const colors = {
  cyan: (s: string) => `\x1b[36m${s}\x1b[0m`,
  green: (s: string) => `\x1b[32m${s}\x1b[0m`,
  yellow: (s: string) => `\x1b[33m${s}\x1b[0m`,
  red: (s: string) => `\x1b[31m${s}\x1b[0m`,
  dim: (s: string) => `\x1b[2m${s}\x1b[0m`,
  bold: (s: string) => `\x1b[1m${s}\x1b[0m`,
};

export function step(name: string): void {
  console.log("\n" + colors.bold(colors.cyan(`▸ ${name}`)));
}

export function info(msg: string): void {
  console.log(colors.dim("  ") + msg);
}

export function ok(msg: string): void {
  console.log(colors.dim("  ") + colors.green("✓") + " " + msg);
}

export function fail(msg: string): never {
  console.error(colors.red(`error: ${msg}`));
  Deno.exit(1);
}

// ---------------------------------------------------------------------------
// file utils

export async function exists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch {
    return false;
  }
}

export async function copyFile(src: string, dst: string): Promise<void> {
  await ensureDir(dirname(dst));
  await copy(src, dst, { overwrite: true });
}

export function rel(p: string): string {
  return p.startsWith(REPO_ROOT) ? p.slice(REPO_ROOT.length + 1) : p;
}

export function humanBytes(n: number): string {
  if (n < 1024) return `${n}B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)}KB`;
  return `${(n / (1024 * 1024)).toFixed(1)}MB`;
}

export async function sha256File(path: string): Promise<{ sha256: string; size: number }> {
  const data = await Deno.readFile(path);
  const digest = await crypto.subtle.digest("SHA-256", data.buffer as ArrayBuffer);
  return {
    sha256: encodeHex(new Uint8Array(digest)),
    size: data.byteLength,
  };
}

export async function sha256String(s: string): Promise<string> {
  const data = new TextEncoder().encode(s);
  const digest = await crypto.subtle.digest("SHA-256", data.buffer);
  return encodeHex(new Uint8Array(digest));
}

export function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.byteLength !== b.byteLength) return false;
  for (let i = 0; i < a.byteLength; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

export function decodeBase64(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// ---------------------------------------------------------------------------
// .env handling

export interface DeployEnv {
  signingPrivateKey: Uint8Array;
  signingPublicKey: Uint8Array;
  /** Cloudflare token + account id. The account id also forms the R2 S3 endpoint
   *  host (`<account>.r2.cloudflarestorage.com`). The token authenticates the
   *  landing-page Worker deploy via wrangler (see wranglerEnv); R2 object I/O uses
   *  the S3 credentials below, not this token. Empty when the user relies on a
   *  stored `wrangler login` OAuth session for the Worker deploy. */
  cloudflareApiToken: string;
  cloudflareAccountId: string;
  /** R2 S3-API credentials (an "Object Read & Write" R2 API token) used for every
   *  R2 object upload/copy/delete over the S3-compatible endpoint. */
  r2AccessKeyId: string;
  r2SecretAccessKey: string;
  websiteDomain: string;
  storageDomain: string;
  r2Bucket: string;
  /** Raw text of the Tauri updater keys (minisign format). Empty when the
   *  user hasn't set up client updates yet. The release orchestrator skips the
   *  client item with a yellow warning when these are missing. */
  tauriUpdaterPublicKey: string;
  tauriUpdaterPrivateKey: string;
  tauriUpdaterPassword: string;
  /** Android release keystore (base64 of the .jks) + its passwords/alias. Empty
   *  when the user hasn't set up Android signing yet; the release orchestrator
   *  skips the android item with a yellow warning when the keystore is missing.
   *  Unlike the desktop client, the APK is not self-update-signed by Tauri (the
   *  updater plugin has no Android support); the keystore signs the APK for
   *  install and the Ed25519 signingPrivateKey signs android.json. */
  androidKeystoreB64: string;
  androidKeystorePassword: string;
  androidKeyAlias: string;
  androidKeyPassword: string;
  /** Apple Developer ID signing + notarization for the macOS client bundle. All
   *  optional and empty by default: when unset, the macOS build keeps ad-hoc
   *  signing (tauri.conf.json's signingIdentity "-") and skips notarization, so
   *  the pipeline behaves exactly as before. When filled, buildClient injects
   *  them into the Tauri build env (see client.ts). Tauri reads the identity from
   *  APPLE_SIGNING_IDENTITY (or the base64 .p12 in APPLE_CERTIFICATE +
   *  APPLE_CERTIFICATE_PASSWORD on CI) and notarizes with either the Apple ID
   *  trio (appleId/applePassword/appleTeamId) or the App Store Connect API key
   *  trio (appleApiKey/appleApiIssuer/appleApiKeyPath). macOS builds host-natively
   *  (Tauri can't cross-sign), so these ride only on a macOS host/runner. */
  appleSigningIdentity: string;
  appleCertificateB64: string;
  appleCertificatePassword: string;
  appleId: string;
  applePassword: string;
  appleTeamId: string;
  appleApiKey: string;
  appleApiIssuer: string;
  appleApiKeyPath: string;
  /** base64 of the App Store Connect .p8, primarily for CI: a runner cannot
   *  carry a file path as a secret, so the release entry points materialize this
   *  to a temp file and fill appleApiKeyPath themselves (see
   *  materializeAppleApiKey in apple-toolchain.ts). Locally prefer leaving it
   *  empty and pointing APPLE_API_KEY_PATH at the real .p8. */
  appleApiKeyB64: string;
  /** Developer ID INSTALLER identity for productsign (the macOS .pkg
   *  installer). Distinct from appleSigningIdentity: that is an Application
   *  cert, which productsign rejects. When unset the .pkg ships unsigned (with
   *  a note), exactly as before. The base64 .p12 (+ password) is CI-only: it is imported into an
   *  ephemeral keychain for the productsign run (see materializeInstallerKeychain
   *  in apple-toolchain.ts); locally leave both empty and rely on the cert in
   *  the login keychain. */
  appleInstallerSigningIdentity: string;
  appleInstallerCertificateB64: string;
  appleInstallerCertificatePassword: string;
  /** Windows Authenticode code-signing, DORMANT by default (no cert yet). When
   *  `windowsCertificateThumbprint` (or `windowsSignCommand`) is set, the client
   *  NSIS installer + the Core NSIS installer are Authenticode-signed at build
   *  time; when empty the installers ship unsigned and the install flow keeps
   *  stripping Mark-of-the-Web. Mirrors the inert-by-default Apple signing, but
   *  Tauri reads Windows signing from tauri.conf.json (not env), so it is applied
   *  as a build-time config patch (see injectWindowsSigning in client.ts). */
  windowsCertificateThumbprint: string;
  windowsSignCommand: string;
  windowsTimestampUrl: string;
}

export async function loadOrSeedEnv(): Promise<DeployEnv> {
  if (!(await exists(ENV_PATH))) {
    if (!(await exists(ENV_EXAMPLE_PATH))) {
      fail(`neither .env nor .env.example found at ${REPO_ROOT}`);
    }
    info(`.env missing; seeding from .env.example`);
    await copyFile(ENV_EXAMPLE_PATH, ENV_PATH);
  }

  const raw = await loadDotenv({ envPath: ENV_PATH, export: false });
  const get = (k: string) => raw[k] ?? "";

  let privB64 = get("LOCA_SIGNING_PRIVATE_KEY_B64");
  let pubB64 = get("LOCA_SIGNING_PUBLIC_KEY_B64");

  if (!privB64 && !pubB64) {
    info(`generating new Ed25519 signing keypair`);
    const sk = ed.utils.randomSecretKey();
    const pk = await ed.getPublicKeyAsync(sk);
    privB64 = encodeBase64(sk);
    pubB64 = encodeBase64(pk);
    await persistEnvKey("LOCA_SIGNING_PRIVATE_KEY_B64", privB64);
    await persistEnvKey("LOCA_SIGNING_PUBLIC_KEY_B64", pubB64);
    ok(`keypair written to .env`);
  } else if (!privB64 || !pubB64) {
    fail(
      `one half of the signing keypair is missing in .env. Either fill both ` +
        `or clear both (and re-run to generate)`,
    );
  } else {
    const sk = decodeBase64(privB64);
    const derivedPk = encodeBase64(await ed.getPublicKeyAsync(sk));
    if (derivedPk !== pubB64) {
      fail(
        `signing keypair in .env is mismatched: derived public key ` +
          `${derivedPk} != stored ${pubB64}`,
      );
    }
  }

  const tauriPub = get("TAURI_UPDATER_PUBLIC_KEY");
  const tauriPriv = get("TAURI_UPDATER_PRIVATE_KEY");
  if (tauriPub && !tauriPriv) {
    fail(
      `TAURI_UPDATER_PUBLIC_KEY is set but TAURI_UPDATER_PRIVATE_KEY is not. ` +
        `Either fill both or clear both. Re-generate via ` +
        `\`cargo tauri signer generate -w .env\`.`,
    );
  }

  return {
    signingPrivateKey: decodeBase64(privB64),
    signingPublicKey: decodeBase64(pubB64),
    cloudflareApiToken: get("CLOUDFLARE_API_TOKEN"),
    cloudflareAccountId: get("CLOUDFLARE_ACCOUNT_ID"),
    r2AccessKeyId: get("LOCA_R2_ACCESS_KEY_ID"),
    r2SecretAccessKey: get("LOCA_R2_SECRET_ACCESS_KEY"),
    websiteDomain: get("LOCA_WEBSITE_DOMAIN") || "loca.rzkyif.com",
    storageDomain: get("LOCA_STORAGE_DOMAIN") || "get.loca.rzkyif.com",
    r2Bucket: get("LOCA_R2_BUCKET") || "loca-releases",
    tauriUpdaterPublicKey: tauriPub,
    tauriUpdaterPrivateKey: tauriPriv,
    tauriUpdaterPassword: get("TAURI_UPDATER_PRIVATE_KEY_PASSWORD"),
    androidKeystoreB64: get("LOCA_ANDROID_KEYSTORE_B64"),
    androidKeystorePassword: get("LOCA_ANDROID_KEYSTORE_PASSWORD"),
    androidKeyAlias: get("LOCA_ANDROID_KEY_ALIAS") || "upload",
    androidKeyPassword: get("LOCA_ANDROID_KEY_PASSWORD") || get("LOCA_ANDROID_KEYSTORE_PASSWORD"),
    appleSigningIdentity: get("APPLE_SIGNING_IDENTITY"),
    appleCertificateB64: get("APPLE_CERTIFICATE"),
    appleCertificatePassword: get("APPLE_CERTIFICATE_PASSWORD"),
    appleId: get("APPLE_ID"),
    applePassword: get("APPLE_PASSWORD"),
    appleTeamId: get("APPLE_TEAM_ID"),
    appleApiKey: get("APPLE_API_KEY"),
    appleApiIssuer: get("APPLE_API_ISSUER"),
    appleApiKeyPath: get("APPLE_API_KEY_PATH"),
    appleApiKeyB64: get("APPLE_API_KEY_B64"),
    appleInstallerSigningIdentity: get("APPLE_INSTALLER_SIGNING_IDENTITY"),
    appleInstallerCertificateB64: get("APPLE_INSTALLER_CERTIFICATE"),
    appleInstallerCertificatePassword: get("APPLE_INSTALLER_CERTIFICATE_PASSWORD"),
    windowsCertificateThumbprint: get("WINDOWS_CERTIFICATE_THUMBPRINT"),
    windowsSignCommand: get("WINDOWS_SIGN_COMMAND"),
    windowsTimestampUrl: get("WINDOWS_TIMESTAMP_URL") || "http://timestamp.digicert.com",
  };
}

/** The minimal DeployEnv a BUILD environment needs, constructed directly from
 *  the process env rather than via loadOrSeedEnv: only the build-time signing
 *  fields (Tauri minisign, Android keystore) and the Ed25519 PUBLIC key. The
 *  Ed25519 PRIVATE key is left empty, so it never reaches a build runner
 *  (manifest signing happens only on the publish coordinator). Used by the CI
 *  build half (ci-build.ts) and the preflight gate (ci-preflight.ts). */
export function envFromProcess(): DeployEnv {
  const get = (k: string) => Deno.env.get(k) ?? "";
  const pubB64 = get("LOCA_SIGNING_PUBLIC_KEY_B64");
  if (!pubB64) {
    fail(`LOCA_SIGNING_PUBLIC_KEY_B64 is required (baked into the core binary for verification).`);
  }
  return {
    signingPrivateKey: new Uint8Array(),
    signingPublicKey: decodeBase64(pubB64),
    cloudflareApiToken: "",
    cloudflareAccountId: "",
    r2AccessKeyId: "",
    r2SecretAccessKey: "",
    websiteDomain: get("LOCA_WEBSITE_DOMAIN") || "loca.rzkyif.com",
    storageDomain: get("LOCA_STORAGE_DOMAIN") || "get.loca.rzkyif.com",
    r2Bucket: get("LOCA_R2_BUCKET") || "loca-releases",
    tauriUpdaterPublicKey: get("TAURI_UPDATER_PUBLIC_KEY"),
    tauriUpdaterPrivateKey: get("TAURI_UPDATER_PRIVATE_KEY"),
    tauriUpdaterPassword: get("TAURI_UPDATER_PRIVATE_KEY_PASSWORD"),
    androidKeystoreB64: get("LOCA_ANDROID_KEYSTORE_B64"),
    androidKeystorePassword: get("LOCA_ANDROID_KEYSTORE_PASSWORD"),
    androidKeyAlias: get("LOCA_ANDROID_KEY_ALIAS") || "upload",
    androidKeyPassword: get("LOCA_ANDROID_KEY_PASSWORD") || get("LOCA_ANDROID_KEYSTORE_PASSWORD"),
    appleSigningIdentity: get("APPLE_SIGNING_IDENTITY"),
    appleCertificateB64: get("APPLE_CERTIFICATE"),
    appleCertificatePassword: get("APPLE_CERTIFICATE_PASSWORD"),
    appleId: get("APPLE_ID"),
    applePassword: get("APPLE_PASSWORD"),
    appleTeamId: get("APPLE_TEAM_ID"),
    appleApiKey: get("APPLE_API_KEY"),
    appleApiIssuer: get("APPLE_API_ISSUER"),
    appleApiKeyPath: get("APPLE_API_KEY_PATH"),
    appleApiKeyB64: get("APPLE_API_KEY_B64"),
    appleInstallerSigningIdentity: get("APPLE_INSTALLER_SIGNING_IDENTITY"),
    appleInstallerCertificateB64: get("APPLE_INSTALLER_CERTIFICATE"),
    appleInstallerCertificatePassword: get("APPLE_INSTALLER_CERTIFICATE_PASSWORD"),
    windowsCertificateThumbprint: get("WINDOWS_CERTIFICATE_THUMBPRINT"),
    windowsSignCommand: get("WINDOWS_SIGN_COMMAND"),
    windowsTimestampUrl: get("WINDOWS_TIMESTAMP_URL") || "http://timestamp.digicert.com",
  };
}

/** In-place .env editor that preserves comments and ordering. Falls back to
 *  appending the key at the bottom if not present. */
export async function persistEnvKey(key: string, value: string): Promise<void> {
  const text = await Deno.readTextFile(ENV_PATH);
  const lines = text.split("\n");
  let found = false;
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].startsWith(`${key}=`)) {
      lines[i] = `${key}=${value}`;
      found = true;
      break;
    }
  }
  if (!found) lines.push(`${key}=${value}`);
  await Deno.writeTextFile(ENV_PATH, lines.join("\n"));
}

// ---------------------------------------------------------------------------
// signing-keys.json regeneration

export async function writeSigningKeys(publicKeyB64: string): Promise<void> {
  const json = JSON.stringify({ publicKey: publicKeyB64 }, null, 2) + "\n";
  await Deno.writeTextFile(SIGNING_KEYS_PATH, json);
  ok(`wrote public key to ${rel(SIGNING_KEYS_PATH)}`);
}

// ---------------------------------------------------------------------------
// item versions

export async function readCoreVersion(): Promise<string> {
  const rust = await readRustCoreVersion();
  const ts = await readTsCoreVersion();
  // Refused rather than reconciled. The manifest is signed with ONE version and
  // the binary reports the other; if they disagree, an update check compares a
  // manifest version against a core that calls itself something else and either
  // re-offers an update forever or never offers one again. Both are silent.
  //
  // They must also agree for the conformance suite: `version` is on
  // `GET /api/v1/health`, so a differential run would report the port and the
  // oracle as disagreeing about a field neither implements wrongly.
  if (rust !== ts) {
    fail(
      `CORE_VERSION disagrees: ${rel(CORE_CONFIG_RS)} says ${rust}, ` +
        `${rel(CONFIG_PATH)} says ${ts}. The Rust value is what ships; bump both.`,
    );
  }
  return rust;
}

async function readRustCoreVersion(): Promise<string> {
  const text = await Deno.readTextFile(CORE_CONFIG_RS);
  const match = text.match(/pub\s+const\s+CORE_VERSION\s*:\s*&str\s*=\s*"([^"]+)"/);
  if (!match) fail(`could not parse CORE_VERSION from ${rel(CORE_CONFIG_RS)}`);
  return match[1];
}

async function readTsCoreVersion(): Promise<string> {
  const text = await Deno.readTextFile(CONFIG_PATH);
  const match = text.match(/export\s+const\s+CORE_VERSION\s*=\s*"([^"]+)"/);
  if (!match) fail(`could not parse CORE_VERSION from ${rel(CONFIG_PATH)}`);
  return match[1];
}

/** Read the top-level `version` field from a JSON file (deno.json, the
 *  schema, …). Used by the versioned release items. */
export async function readVersionField(path: string): Promise<string> {
  const cfg = JSON.parse(await Deno.readTextFile(path)) as { version?: string };
  if (!cfg.version) fail(`no "version" field in ${rel(path)}`);
  return cfg.version;
}

/** Return `v` with its patch component incremented (major.minor stay put). */
export function bumpPatch(v: string): string {
  const [maj, min, patch] = parseSemver(v);
  return `${maj}.${min}.${patch + 1}`;
}

/** Surgically bump `CORE_VERSION` in config.ts to the next patch, preserving the
 *  rest of the file byte-for-byte. Returns the new version. */
export async function bumpCoreVersion(): Promise<string> {
  // Reads through the agreement check first, so a tree that has already drifted
  // is refused here rather than bumped into a wider drift.
  const next = bumpPatch(await readCoreVersion());

  const rustRe = /(pub\s+const\s+CORE_VERSION\s*:\s*&str\s*=\s*")([^"]+)(")/;
  const rustText = await Deno.readTextFile(CORE_CONFIG_RS);
  if (!rustRe.test(rustText)) fail(`could not parse CORE_VERSION from ${rel(CORE_CONFIG_RS)}`);
  await Deno.writeTextFile(CORE_CONFIG_RS, rustText.replace(rustRe, `$1${next}$3`));

  // The oracle follows. It ships nowhere, but conformance compares its
  // `/health` against the port's, so leaving it behind would turn every
  // differential run red on a field that is not a defect.
  const tsRe = /(export\s+const\s+CORE_VERSION\s*=\s*")([^"]+)(")/;
  const tsText = await Deno.readTextFile(CONFIG_PATH);
  if (!tsRe.test(tsText)) fail(`could not parse CORE_VERSION from ${rel(CONFIG_PATH)}`);
  await Deno.writeTextFile(CONFIG_PATH, tsText.replace(tsRe, `$1${next}$3`));

  return next;
}

/** Surgically bump the top-level `"version"` field of a JSON file to the next
 *  patch, anchored on its current value so formatting and every other field are
 *  untouched. Returns the new version. */
export async function bumpVersionField(path: string): Promise<string> {
  const text = await Deno.readTextFile(path);
  const current = await readVersionField(path);
  const next = bumpPatch(current);
  const re = new RegExp(`("version"\\s*:\\s*")${escapeRegExp(current)}(")`);
  if (!re.test(text)) fail(`could not locate "version": "${current}" in ${rel(path)}`);
  await Deno.writeTextFile(path, text.replace(re, `$1${next}$2`));
  return next;
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// ---------------------------------------------------------------------------
// version normalization for source hashing
//
// A release item's version lives INSIDE its hashed source (CORE_VERSION in
// config.ts, "version" in deno.json / tauri.conf.json / the schema). Left as-is,
// the post-release bump of that file would change the item's source hash and
// re-trigger a full rebuild + republish on the next promotion, with no real
// source change. These normalizers blank the version to a fixed placeholder
// before hashing so a version-only bump is invisible to change detection. The
// version still ships (in the manifest) and still gates a release (the
// version-bump gate reads version() directly); it just never TRIGGERS one. The
// items that keep the version OUT of their hashed tree entirely - install
// scripts (version.json excluded) and the catalog (version-free payload) - need
// no normalizer.

const VERSION_HASH_PLACEHOLDER = "0.0.0";

/** Return `text` with the top-level JSON `"version"` value replaced by a fixed
 *  placeholder. Anchored on the parsed top-level value exactly like
 *  bumpVersionField, so a same-valued nested `"version"` is never touched.
 *  Returns the text unchanged when there is no parseable top-level version. */
export function stripJsonVersion(text: string): string {
  let current: string | undefined;
  try {
    current = (JSON.parse(text) as { version?: string }).version;
  } catch {
    return text;
  }
  if (!current) return text;
  const re = new RegExp(`("version"\\s*:\\s*")${escapeRegExp(current)}(")`);
  return text.replace(re, `$1${VERSION_HASH_PLACEHOLDER}$2`);
}

/** The config.ts counterpart: blank the `CORE_VERSION = "x.y.z"` constant. */
export function stripCoreVersion(text: string): string {
  // Matches BOTH spellings: `export const CORE_VERSION = "..."` (TypeScript) and
  // `pub const CORE_VERSION: &str = "..."` (Rust). The optional type ascription
  // is the whole difference, and missing it would leave the Rust version in the
  // tree hash so that every bump looked like a source change.
  return text.replace(
    /(CORE_VERSION\s*(?::\s*&str\s*)?=\s*")[^"]+(")/,
    `$1${VERSION_HASH_PLACEHOLDER}$2`,
  );
}

// ---------------------------------------------------------------------------
// canonical JSON for signing. Single-sourced in @loca/shared (imported above)
// so the signer here and the core verifiers (binaries/manifest.ts,
// update/self-updater.ts) recompute byte-identical bytes and can't drift.
export { canonicalize };

// ---------------------------------------------------------------------------
// host triple detection

import type { Triple } from "../../packages/loca-shared/src/domain/model.ts";

export function detectHostTriple(): Triple {
  const os = Deno.build.os;
  const arch = Deno.build.arch;
  const arched = arch === "aarch64" ? "aarch64" : "x86_64";
  if (os === "darwin") return `${arched}-apple-darwin` as Triple;
  if (os === "linux") return `${arched}-unknown-linux-gnu` as Triple;
  if (os === "windows") return `${arched}-pc-windows-msvc` as Triple;
  fail(`unsupported host OS: ${os}`);
}

// ---------------------------------------------------------------------------
// release channels
//
// stable + latest are the only *published* channels (dev is local-only). The
// conventions mirror the runtime side (core paths.ts channelSuffix +
// config.ts manifestDir, client channel.rs): stable stays bare for
// back-compat; latest suffixes our binary names and nests manifests + artifacts
// under a /latest path segment.

export type ReleaseChannel = "stable" | "latest";

export function parseChannelFlag(value: string | undefined): ReleaseChannel {
  const ch = (value ?? "stable").trim() || "stable";
  if (ch !== "stable" && ch !== "latest") {
    fail(`invalid --channel: ${ch} (expected "stable" or "latest")`);
  }
  return ch;
}

/** Suffix on Loca's own binary names: "" for stable, "-latest" otherwise. */
export function channelBinSuffix(channel: ReleaseChannel): string {
  return channel === "stable" ? "" : `-${channel}`;
}

/** R2 manifest dir for this channel: "manifests" | "manifests/latest". */
export function channelManifestDir(channel: ReleaseChannel): string {
  return channel === "stable" ? "manifests" : `manifests/${channel}`;
}

/** R2 artifact key prefix before "<version>/<triple>/…": "" | "latest/". */
export function channelStoragePrefix(channel: ReleaseChannel): string {
  return channel === "stable" ? "" : `${channel}/`;
}

// ---------------------------------------------------------------------------
// astro + wrangler

/** Subprocess env additions so the spawned wrangler authenticates with the
 *  CLOUDFLARE_* credentials from .env. loadDotenv runs with export:false, so the
 *  token never reaches Deno.env and a child wouldn't inherit it; we hand it to
 *  wrangler explicitly here. Returns undefined when nothing is set, in which
 *  case wrangler falls back to its stored `wrangler login` OAuth session. */
export function wranglerEnv(env: DeployEnv): Record<string, string> | undefined {
  const out: Record<string, string> = {};
  if (env.cloudflareApiToken) out.CLOUDFLARE_API_TOKEN = env.cloudflareApiToken;
  if (env.cloudflareAccountId) out.CLOUDFLARE_ACCOUNT_ID = env.cloudflareAccountId;
  return Object.keys(out).length > 0 ? out : undefined;
}

export async function astroBuild(): Promise<void> {
  const cmd = new Deno.Command("deno", {
    args: ["run", "-A", "npm:astro@^7", "build"],
    cwd: WEBSITE_DIR,
    stdout: "inherit",
    stderr: "inherit",
  });
  const { code } = await cmd.output();
  if (code !== 0) fail(`astro build exited ${code}`);
}

export async function wranglerDeploy(env: DeployEnv): Promise<void> {
  const cmd = new Deno.Command("deno", {
    args: ["run", "-A", "npm:wrangler@^4", "deploy"],
    cwd: WEBSITE_DIR,
    env: wranglerEnv(env),
    stdout: "inherit",
    stderr: "inherit",
  });
  const { code } = await cmd.output();
  if (code !== 0) fail(`wrangler deploy exited ${code}`);
}

// ---------------------------------------------------------------------------
// R2 uploads (S3-compatible API)
//
// Object I/O goes straight to R2's S3 endpoint over signed `fetch`, not through
// `wrangler r2 object`: a subprocess per object paid a ~1.5s deno+wrangler
// cold-start each, and with 100+ objects per publish that dominated the job.
// In-process requests have no cold-start and run concurrently (mapPool), aliases
// become server-side copies instead of a second full upload, and the prune is a
// fan-out of quick DELETEs. wrangler stays only for the Worker deploy.

/** Bounded-concurrency map: runs `fn` over `items` with at most `concurrency`
 *  requests in flight, preserving result order. Overlaps the many independent R2
 *  round-trips instead of serializing them. */
export async function mapPool<T, R>(
  items: readonly T[],
  concurrency: number,
  fn: (item: T, index: number) => Promise<R>,
): Promise<R[]> {
  const results = Array.from<R>({ length: items.length });
  let next = 0;
  const worker = async () => {
    while (true) {
      const i = next++;
      if (i >= items.length) return;
      results[i] = await fn(items[i], i);
    }
  };
  const n = Math.max(1, Math.min(concurrency, items.length));
  await Promise.all(Array.from({ length: n }, () => worker()));
  return results;
}

/** Max R2 object requests in flight at once. Large artifacts (~150MB) are read
 *  into memory to be signed, so this also bounds peak memory (cap * file size). */
export const R2_CONCURRENCY = 4;

function r2Client(env: DeployEnv): AwsClient {
  if (!env.r2AccessKeyId || !env.r2SecretAccessKey) {
    fail(
      `R2 S3 credentials missing: set LOCA_R2_ACCESS_KEY_ID + ` +
        `LOCA_R2_SECRET_ACCESS_KEY in .env (an "Object Read & Write" R2 API token).`,
    );
  }
  return new AwsClient({
    accessKeyId: env.r2AccessKeyId,
    secretAccessKey: env.r2SecretAccessKey,
    service: "s3",
    region: "auto",
  });
}

function r2ObjectUrl(env: DeployEnv, key: string): string {
  if (!env.cloudflareAccountId) {
    fail(`CLOUDFLARE_ACCOUNT_ID is required to form the R2 S3 endpoint.`);
  }
  // Release keys are all safe ASCII (versions, triples, filenames), so no
  // per-segment percent-encoding is needed; path-style addressing puts the
  // bucket first.
  return `https://${env.cloudflareAccountId}.r2.cloudflarestorage.com/${env.r2Bucket}/${key}`;
}

/** Per-attempt ceiling for a single R2 object request. Generous enough for a
 *  ~150MB upload on a slow runner uplink, low enough that a stalled connection
 *  fails over to a retry instead of hanging until the job timeout. */
const R2_REQUEST_TIMEOUT_MS = 120_000;

/** Signed R2 request with bounded retry on transient failures (network error,
 *  5xx, 429). Object writes are idempotent (same key + bytes), so a retry is
 *  safe; the AWS SDK behind `wrangler` retried these internally, so this keeps a
 *  single blip mid-upload from aborting the whole publish. A non-transient status
 *  (4xx: auth, bad request) is returned as-is so the caller fails fast with the
 *  response body. Exhausting all attempts aborts loudly. */
async function r2SignedFetch(
  env: DeployEnv,
  url: string,
  init: RequestInit,
  label: string,
): Promise<Response> {
  const client = r2Client(env);
  let lastErr = "";
  for (let attempt = 1; attempt <= 4; attempt++) {
    try {
      const res = await client.fetch(url, {
        ...init,
        signal: AbortSignal.timeout(R2_REQUEST_TIMEOUT_MS),
      });
      if (res.ok || (res.status < 500 && res.status !== 429)) return res;
      lastErr = `${res.status} ${res.statusText}`;
      await res.body?.cancel();
    } catch (err) {
      lastErr = err instanceof Error ? err.message : String(err);
    }
    if (attempt < 4) {
      info(
        colors.yellow(
          `r2 ${label} failed (attempt ${attempt}/4: ${lastErr}); retrying in ${attempt * 2}s`,
        ),
      );
      await new Promise((resolve) => setTimeout(resolve, attempt * 2000));
    }
  }
  fail(`r2 ${label} failed after 4 attempts: ${lastErr}. Re-run the task.`);
}

export async function r2Put(
  env: DeployEnv,
  key: string,
  file: string,
  contentType: string,
  cacheControl?: string,
): Promise<void> {
  const headers: Record<string, string> = { "content-type": contentType };
  if (cacheControl) headers["cache-control"] = cacheControl;
  const body = await Deno.readFile(file);
  const res = await r2SignedFetch(
    env,
    r2ObjectUrl(env, key),
    { method: "PUT", body, headers },
    `put ${key}`,
  );
  const text = await res.text();
  if (!res.ok) fail(`r2 put ${key} failed: ${res.status} ${res.statusText}\n${text}`);
  await verifyR2Upload(env, key, file);
}

/** Server-side copy of an already-uploaded object to a second key, replacing the
 *  stored content-type + cache-control. Used for the version-less `current/`
 *  aliases so the same (often ~150MB) bytes are not re-uploaded a second time. */
export async function r2Copy(
  env: DeployEnv,
  srcKey: string,
  destKey: string,
  localFile: string,
  contentType: string,
  cacheControl?: string,
): Promise<void> {
  const headers: Record<string, string> = {
    "x-amz-copy-source": `/${env.r2Bucket}/${srcKey}`,
    "x-amz-metadata-directive": "REPLACE",
    "content-type": contentType,
  };
  if (cacheControl) headers["cache-control"] = cacheControl;
  const res = await r2SignedFetch(
    env,
    r2ObjectUrl(env, destKey),
    { method: "PUT", headers },
    `copy ${srcKey} -> ${destKey}`,
  );
  const text = await res.text();
  if (!res.ok) {
    fail(`r2 copy ${srcKey} -> ${destKey} failed: ${res.status} ${res.statusText}\n${text}`);
  }
  // The alias must be byte-identical to its source; verify against the local file
  // (this also catches S3's "200 OK with an error in the body" copy quirk).
  await verifyR2Upload(env, destKey, localFile);
}

// Verify an upload actually landed AND matches byte-for-byte: GET the public URL
// with a cachebusting query param and compare sha256 (a Content-Length check
// alone would pass a same-length corruption or count-preserving truncation).
// Guards against a truncated write or stale edge copy after a PUT/COPY.
async function verifyR2Upload(env: DeployEnv, key: string, file: string): Promise<void> {
  const url = `https://${env.storageDomain}/${key}?_v=${Date.now()}`;
  // A freshly-uploaded object can be slow to serve from the edge, and the GET
  // occasionally hangs indefinitely (no server response, no socket error), which
  // silently burned ~20 min per stall in CI. Bound each attempt with a timeout
  // and retry with backoff so a stuck fetch fails fast instead of blocking.
  let remoteBytes: Uint8Array | undefined;
  let lastErr = "";
  for (let attempt = 1; attempt <= 5; attempt++) {
    try {
      const res = await fetch(url, { signal: AbortSignal.timeout(60_000) });
      if (!res.ok || !res.body) {
        lastErr = `GET returned ${res.status} ${res.statusText}`;
      } else {
        remoteBytes = new Uint8Array(await res.arrayBuffer());
        break;
      }
    } catch (err) {
      lastErr = err instanceof Error ? err.message : String(err);
    }
    if (attempt < 5) {
      info(
        colors.yellow(
          `r2 verify ${key} failed (attempt ${attempt}/5: ${lastErr}); retrying in ${attempt * 3}s`,
        ),
      );
      await new Promise((resolve) => setTimeout(resolve, attempt * 3000));
    }
  }
  if (!remoteBytes) {
    fail(
      `r2 put ${key}: post-upload GET failed after 5 attempts (${lastErr}). ` +
        `The upload may not have propagated. Re-run the task.`,
    );
  }
  const remoteHash = encodeHex(
    new Uint8Array(await crypto.subtle.digest("SHA-256", remoteBytes.buffer as ArrayBuffer)),
  );
  const local = await sha256File(file);
  if (remoteBytes.byteLength !== local.size) {
    fail(
      `r2 put ${key}: size mismatch (local ${local.size}, remote ${remoteBytes.byteLength}). ` +
        `Re-run the task.`,
    );
  }
  if (remoteHash !== local.sha256) {
    fail(
      `r2 put ${key}: sha256 mismatch (local ${local.sha256}, remote ${remoteHash}). ` +
        `Re-run the task.`,
    );
  }
}

export async function r2PutInline(
  env: DeployEnv,
  key: string,
  body: string,
  contentType: string,
  cacheControl?: string,
): Promise<void> {
  const tmp = await Deno.makeTempFile({ prefix: "r2-inline-" });
  try {
    await Deno.writeTextFile(tmp, body);
    await r2Put(env, key, tmp, contentType, cacheControl);
  } finally {
    await Deno.remove(tmp).catch(() => {});
  }
}

/** Delete one R2 object. Warns and continues on failure rather than aborting:
 *  pruned objects are already off every live manifest, so a lingering artifact is
 *  harmless, and failing an already-published release over a stale delete is
 *  worse. Returns whether the delete succeeded. */
export async function r2Delete(env: DeployEnv, key: string): Promise<boolean> {
  let res: Response;
  try {
    res = await r2Client(env).fetch(r2ObjectUrl(env, key), { method: "DELETE" });
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    info(colors.yellow(`prune: r2 delete ${key} errored (${msg}); skipping`));
    return false;
  }
  await res.body?.cancel();
  // S3 returns 204 on success and also when the key is already absent.
  if (!res.ok && res.status !== 404) {
    info(colors.yellow(`prune: r2 delete ${key} -> ${res.status}; skipping`));
    return false;
  }
  return true;
}

// ---------------------------------------------------------------------------
// idempotency probes (HTTPS GETs against R2 / Worker)

/** GET an R2-hosted JSON at `https://${storageDomain}/${r2Key}`. Returns null
 *  on 404 (treated as "not yet published"). Throws on other non-2xx. */
export async function fetchLiveJson<T>(env: DeployEnv, r2Key: string): Promise<T | null> {
  return await fetchHttpsJson<T>(`https://${env.storageDomain}/${r2Key}`);
}

/** GET a JSON document at an arbitrary HTTPS URL. Returns null on 404. */
export async function fetchHttpsJson<T>(url: string): Promise<T | null> {
  let res: Response;
  try {
    res = await fetch(url);
  } catch (err) {
    fail(`network error fetching ${url}: ${err instanceof Error ? err.message : err}`);
  }
  if (res.status === 404) return null;
  if (!res.ok) fail(`fetch ${url} returned ${res.status} ${res.statusText}`);
  try {
    return (await res.json()) as T;
  } catch (err) {
    fail(`invalid JSON at ${url}: ${err instanceof Error ? err.message : err}`);
  }
}

/** GET an R2-hosted file body. Returns null on 404. Throws on other non-2xx. */
export async function fetchR2Bytes(env: DeployEnv, r2Key: string): Promise<Uint8Array | null> {
  const url = `https://${env.storageDomain}/${r2Key}`;
  let res: Response;
  try {
    res = await fetch(url);
  } catch (err) {
    fail(`network error fetching ${url}: ${err instanceof Error ? err.message : err}`);
  }
  if (res.status === 404) return null;
  if (!res.ok) fail(`fetch ${url} returned ${res.status} ${res.statusText}`);
  return new Uint8Array(await res.arrayBuffer());
}

// ---------------------------------------------------------------------------
// source hashing
//
// A release item's "is it different from production?" signal is a deterministic
// hash of its source inputs (files + directories), independent of any build
// artifact. The unified release records these per item in the release-state
// cursor (below) and diffs the local hash against the recorded one.

export interface HashInput {
  /** Absolute path to a file or directory to fold into the hash. */
  path: string;
  /** Skip files whose repo-relative path matches (e.g. tests, build output). */
  exclude?: (relPath: string) => boolean;
  /** Optional content normalizer applied to each file's TEXT before it is
   *  hashed. Used to blank a version field (see stripJsonVersion /
   *  stripCoreVersion) so a version-only bump does not change the item's source
   *  hash. Only put this on single-file inputs (the version file), never on a
   *  directory input, since it forces a text read that would corrupt binary
   *  assets under the tree. */
  transform?: (text: string) => string;
}

async function hashOneFile(abs: string, transform?: (text: string) => string): Promise<string> {
  if (transform) return await sha256String(transform(await Deno.readTextFile(abs)));
  return (await sha256File(abs)).sha256;
}

/** Tracked files under a repo directory, absolute paths. Uses `git ls-files` so
 *  the hash sees only committed source, never build artifacts (gen/android,
 *  .svelte-kit, dist, staged assets, ...). A filesystem walk would fold those in,
 *  and since they exist after a local build but not in a fresh CI checkout, the
 *  two would compute different hashes and change detection would never converge.
 *  Content is read from the working tree, so a modified tracked file still counts;
 *  only untracked/ignored files are dropped. */
async function trackedFilesUnder(absDir: string): Promise<string[]> {
  const out = await new Deno.Command("git", {
    args: ["ls-files", "-z", "--", rel(absDir)],
    cwd: REPO_ROOT,
    stdout: "piped",
    stderr: "null",
  }).output();
  if (!out.success) return [];
  return new TextDecoder()
    .decode(out.stdout)
    .split("\0")
    .filter(Boolean)
    .map((p) => join(REPO_ROOT, p));
}

/** Stable sha256 over the sorted (repo-relative-path, file-sha256) pairs of the
 *  tracked source reachable from `inputs`. Missing paths are skipped. */
export async function hashPaths(inputs: HashInput[]): Promise<string> {
  const entries: { path: string; sha: string }[] = [];
  for (const input of inputs) {
    if (!(await exists(input.path))) continue;
    const stat = await Deno.stat(input.path);
    if (stat.isFile) {
      const r = rel(input.path);
      if (input.exclude?.(r)) continue;
      entries.push({ path: r, sha: await hashOneFile(input.path, input.transform) });
    } else if (stat.isDirectory) {
      for (const abs of await trackedFilesUnder(input.path)) {
        if (!(await exists(abs))) continue; // tracked-but-deleted in the working tree
        const r = rel(abs);
        if (input.exclude?.(r)) continue;
        entries.push({ path: r, sha: await hashOneFile(abs, input.transform) });
      }
    }
  }
  entries.sort((a, b) => a.path.localeCompare(b.path));
  return await sha256String(entries.map((e) => `${e.path}\t${e.sha}`).join("\n"));
}

// ---------------------------------------------------------------------------
// packages
//
// A "package" is a unit of development (a workspace member). PACKAGES is the
// release side's view of the same package list the root deno.json `workspace`
// array owns: each package's source dir and its manifest kind. Release items
// reference packages by id (their `packages` field) to declare what they are
// built from, and items whose source hash is exactly "each package's src +
// manifest" derive that hash here via packagesHashInputs.

export type PackageKind = "deno" | "rust";

export const PACKAGES: Record<string, { dir: string; kind: PackageKind }> = {
  shared: { dir: "packages/loca-shared", kind: "deno" },
  "core-engine": { dir: "packages/loca-core-engine", kind: "deno" },
  core: { dir: "packages/loca-core", kind: "deno" },
  client: { dir: "packages/loca-client", kind: "deno" },
  website: { dir: "packages/loca-website", kind: "deno" },
  extension: { dir: "packages/loca-extension-builtin", kind: "deno" },
  catalog: { dir: "packages/loca-model-catalog", kind: "deno" },
  // The crate the shipped core is built from since the cutover.
  "core-rs": { dir: "packages/loca-core-rs", kind: "rust" },
  // Not in any release item's package list today: it is a library the Tauri
  // crate links on mobile, so it reaches users through the CLIENT release
  // rather than the core one. Registered so a future item can name it without
  // re-deriving the path.
  "core-ffi": { dir: "packages/loca-core-ffi", kind: "rust" },
  "core-updater": { dir: "packages/loca-core-updater", kind: "rust" },
  "core-keychain": { dir: "packages/loca-core-keychain", kind: "rust" },
  "core-hwinfo": { dir: "packages/loca-core-hwinfo", kind: "rust" },
  "core-ptyhost": { dir: "packages/loca-core-ptyhost", kind: "rust" },
  "core-speech": { dir: "packages/loca-core-speech", kind: "rust" },
};

/** Source-hash inputs for one package: its source tree + its manifest. */
export function pkgHashInputs(id: string): string[] {
  const p = PACKAGES[id];
  if (!p) throw new Error(`unknown package id: ${id}`);
  const manifest = p.kind === "rust" ? "Cargo.toml" : "deno.json";
  return [`${p.dir}/src`, `${p.dir}/${manifest}`];
}

/** Combined source-hash inputs for a set of packages. Order-independent: the
 *  hash sorts paths globally, so [a, b] and [b, a] yield the same hash. */
export function packagesHashInputs(ids: string[]): string[] {
  return ids.flatMap(pkgHashInputs);
}

const WEBSITE_HASH_INPUTS = ["src", "public", "astro.config.mjs", "wrangler.toml"];

export async function hashWebsiteSource(): Promise<string> {
  return await hashPaths(
    WEBSITE_HASH_INPUTS.map((p) => ({
      path: join(WEBSITE_DIR, p),
      // A stale release-state.json from an older website build must not feed
      // back into the hash; the cursor lives on R2 now, not in public/.
      exclude: (r) => r.endsWith("release-state.json"),
    })),
  );
}

// ---------------------------------------------------------------------------
// semver compare (version-bump gate)

function parseSemver(v: string): [number, number, number] {
  const core = v.split("+")[0].split("-")[0];
  const parts = core.split(".").map((n) => parseInt(n, 10) || 0);
  return [parts[0] ?? 0, parts[1] ?? 0, parts[2] ?? 0];
}

/** True when `a` is a strictly higher version than `b` (major.minor.patch;
 *  pre-release/build metadata ignored). */
export function semverGt(a: string, b: string): boolean {
  const pa = parseSemver(a);
  const pb = parseSemver(b);
  for (let i = 0; i < 3; i++) {
    if (pa[i] !== pb[i]) return pa[i] > pb[i];
  }
  return false;
}

// ---------------------------------------------------------------------------
// release-state cursor
//
// One tooling-only JSON on R2 recording each item's {version?, sourceHash} as
// of its last successful release. NEVER fetched or verified by core/client, so
// it carries no signature and has no Zod schema. Channel-specific items
// (core/extension/catalog/client) nest under channels.<channel>; the
// channel-independent items (scripts/schemas/website) live under `shared`.

export const RELEASE_STATE_KEY = "manifests/release-state.json";

/** Versioned artifacts kept on R2 per item per channel; older ones are pruned. */
export const VERSION_RETENTION = 2;

export interface VersionEntry {
  version: string;
  /** Full bucket keys (WITH channel prefix) uploaded for this version. */
  keys: string[];
}

export interface ItemState {
  version?: string;
  sourceHash: string;
  /** Newest-first published-version history, capped at VERSION_RETENTION.
   *  Absent for latest-only items (manifests/schemas/scripts). */
  history?: VersionEntry[];
}

export interface ReleaseCursor {
  schemaVersion: 1;
  channels: Record<string, Record<string, ItemState>>;
  shared: Record<string, ItemState>;
}

/** Fold a freshly published version's uploaded keys into an item's history and
 *  report which keys fall outside the retention window. A version already in the
 *  history (platform-fill / force republish) has its keys unioned in place, so it
 *  is not re-ordered and never self-evicts; a genuinely new version goes to the
 *  front (the version-bump gate guarantees it is the highest, so front == newest).
 *  Pure and I/O-free so it can be unit-tested without a release. */
export function mergeVersionHistory(
  history: VersionEntry[] | undefined,
  version: string,
  keys: string[],
  cap: number,
): { history: VersionEntry[]; evictedKeys: string[] } {
  const prior = history ?? [];
  const idx = prior.findIndex((e) => e.version === version);
  let next: VersionEntry[];
  if (idx >= 0) {
    next = [...prior];
    next[idx] = { version, keys: [...new Set([...prior[idx].keys, ...keys])] };
  } else {
    next = [{ version, keys: [...new Set(keys)] }, ...prior];
  }
  const kept = next.slice(0, cap);
  const evicted = next.slice(cap);
  return { history: kept, evictedKeys: evicted.flatMap((e) => e.keys) };
}

export async function readReleaseCursor(env: DeployEnv): Promise<ReleaseCursor> {
  const live = await fetchLiveJson<ReleaseCursor>(env, RELEASE_STATE_KEY);
  if (live && live.channels && live.shared) return live;
  return { schemaVersion: 1, channels: {}, shared: {} };
}

export async function writeReleaseCursor(env: DeployEnv, cursor: ReleaseCursor): Promise<void> {
  await r2PutInline(
    env,
    RELEASE_STATE_KEY,
    JSON.stringify(cursor, null, 2),
    "application/json",
    "public, max-age=60",
  );
}

// ---------------------------------------------------------------------------
// y/N confirmation

/** Reads a single y/N answer from the TTY. Returns true only on y / yes. */
export function promptYesNo(message: string): boolean {
  const ans = prompt(`${message} (y/N)`);
  return ans !== null && /^y(es)?$/i.test(ans.trim());
}

// ---------------------------------------------------------------------------
// release item contract
//
// Each module under scripts/release/ exports one ReleaseItem. The orchestrator
// (ci-publish.ts) computes sourceHash() to decide what changed, enforces the
// version-bump gate via version()/bumpHint, and calls apply() for changed
// items after confirmation.

export interface ApplyOpts {
  /** Pre-built artifacts collected from CI build runners: the core/client/android
   *  items compose+sign+upload these rather than building anything themselves
   *  (the publish half of the CI build/publish split). The platform-independent
   *  items (extension, catalog, schemas, website) build on the coordinator and
   *  ignore this. */
  prebuilt: PrebuiltStaging;
  /** Build + sign locally but skip every R2 upload. */
  dryRun: boolean;
  /** Versioned items append each uploaded versioned key (full key, WITH channel
   *  prefix) here so the orchestrator can record it in the cursor and prune older
   *  versions. Latest-only items (manifests/schemas/scripts) ignore it. */
  recordVersionedKey?(key: string): void;
  /** Items append the local file + flat asset name of each artifact they want
   *  mirrored to the GitHub Release (installers, APKs, binaries). The
   *  orchestrator uploads them after R2 when githubRelease is on. Manifests are
   *  collected separately by the publisher. */
  recordReleaseAsset?(localPath: string, assetName: string): void;
}

export interface ReleaseItem {
  id: string;
  label: string;
  /** channel-specific items publish per channel; shared items publish once. */
  scope: "channel" | "shared";
  /** Package ids (keys of PACKAGES) this item is built from. A release item is
   *  a unit of distribution and may compose several packages (e.g. `core`).
   *  This is the explicit package -> release-item mapping; for items whose
   *  source hash is exactly "each package's src + manifest" it also derives the
   *  hash (see packagesHashInputs), so the package list cannot drift. */
  packages: string[];
  /** Where to bump the version when this item changed without a bump. */
  bumpHint?: string;
  /** Deterministic hash of this item's source inputs (no compiling). */
  sourceHash(channel: ReleaseChannel): Promise<string>;
  /** Current local version. */
  version(): Promise<string>;
  /** Absolute path of the file holding the version. Doubles as the dedupe key
   *  for the post-release bump: items sharing a file (client + android both use
   *  tauri.conf.json) bump it exactly once. */
  versionFile: string;
  /** Bump `versionFile`'s version to the next patch (uncommitted) after a
   *  successful release, so the next cycle publishes the following version.
   *  Returns the new version. */
  bumpVersion(): Promise<string>;
  /** Extra "changed" signal beyond a source-hash diff (e.g. a client platform
   *  not yet published at the current version). Never gates a version bump. */
  extraChanged?(env: DeployEnv, channel: ReleaseChannel): Promise<boolean>;
  /** Absolute paths (files or dirs) this item compiles to. The unified
   *  `deno task build` hashes them so a wiped or swapped artifact forces a
   *  rebuild even when source is unchanged. Buildable items only. */
  buildOutputs?(channel: ReleaseChannel): Promise<string[]>;
  /** Build + sign + upload. Runs only for changed items, only after confirm. */
  apply(env: DeployEnv, channel: ReleaseChannel, opts: ApplyOpts): Promise<void>;
}

// ---------------------------------------------------------------------------
// signing convenience

export async function signEd25519(privateKey: Uint8Array, body: unknown): Promise<string> {
  const sig = await ed.signAsync(new TextEncoder().encode(canonicalize(body)), privateKey);
  return encodeBase64(sig);
}

/** Detached Ed25519 signature over raw bytes (no canonicalization), base64.
 *  For a file an installer verifies exactly as downloaded (e.g. client.json.sig
 *  over the client.json bytes), where no shared canonicalizer is available. */
export async function signEd25519Bytes(privateKey: Uint8Array, bytes: Uint8Array): Promise<string> {
  return encodeBase64(await ed.signAsync(bytes, privateKey));
}

// ---------------------------------------------------------------------------
// plan + apply orchestration
//
// Shared by the umbrella local release (main.ts, all items) and the CI publish
// half (ci-publish.ts, the items a runner staged): read the cursor, diff each
// item, gate changed-but-unbumped items, confirm, apply, and write the cursor
// back. Callers pre-filter the item list and supply the channel + flags.

export interface RunReleaseOpts {
  yes: boolean;
  dryRun: boolean;
  /** Pre-built artifacts from CI build runners (the publish half of the CI
   *  build/publish split). Passed to each item's apply as opts.prebuilt. */
  prebuilt: PrebuiltStaging;
  /** Mirror this run's artifacts + manifests to the rolling per-channel GitHub
   *  Release after the R2 upload. */
  githubRelease?: boolean;
  /** Extra GitHub-Release assets built outside the release plan (the native Core
   *  installers ci-publish uploads before the plan). Merged into the mirror's
   *  asset set alongside the item-recorded ones. */
  extraGithubAssets?: Array<{ path: string; name: string }>;
}

/** One just-published item, as ci-bump.ts hands it to commitVersionBump. */
export interface PublishedItem {
  id: string;
  label: string;
  versionFile: string;
  /** The source hash itself changed. A platform-fill republish (false) is
   *  republished at the same version and is not bumped. */
  sourceChanged: boolean;
  /** The version just published (the pre-bump value). */
  version: string;
}

interface PlanEntry {
  item: ReleaseItem;
  changed: boolean;
  /** the source hash itself changed (gates a version bump). */
  sourceChanged: boolean;
  localVersion: string;
  recordedVersion?: string;
  localHash: string;
  /** human description shown in the plan table. */
  desc: string;
}

function cursorState(
  cursor: ReleaseCursor,
  item: ReleaseItem,
  channel: ReleaseChannel,
): { version?: string; sourceHash: string } | undefined {
  return item.scope === "shared" ? cursor.shared[item.id] : cursor.channels[channel]?.[item.id];
}

async function planItem(
  env: DeployEnv,
  item: ReleaseItem,
  channel: ReleaseChannel,
  cursor: ReleaseCursor,
): Promise<PlanEntry> {
  const localHash = await item.sourceHash(channel);
  const localVersion = await item.version();
  const recorded = cursorState(cursor, item, channel);
  const sourceChanged = !recorded || recorded.sourceHash !== localHash;
  const extra = item.extraChanged ? await item.extraChanged(env, channel) : false;
  const changed = sourceChanged || extra;

  let desc: string;
  if (!changed) {
    desc = "up to date";
  } else if (sourceChanged) {
    desc = `v${recorded?.version ?? "none"} → v${localVersion}`;
  } else {
    // changed only because a platform is missing at the current version
    desc = `v${localVersion} (publish missing platform)`;
  }

  return {
    item,
    changed,
    sourceChanged,
    localVersion,
    recordedVersion: recorded?.version,
    localHash,
    desc,
  };
}

/** True when any item differs from what the channel has published (the same
 *  change detection runReleasePlan uses, without building or prompting). Used by
 *  the CI preflight gate to skip the build/publish jobs when a branch push has
 *  nothing to do (e.g. a local release already published this commit). */
export async function hasReleaseChanges(
  env: DeployEnv,
  items: ReleaseItem[],
  channel: ReleaseChannel,
): Promise<boolean> {
  const cursor = await readReleaseCursor(env);
  for (const item of items) {
    const plan = await planItem(env, item, channel, cursor);
    if (plan.changed) return true;
  }
  return false;
}

/** Read the cursor, plan each item, enforce the version-bump gate, confirm, and
 *  apply + record the changed items. Returns the number of items published (0
 *  when nothing changed or the user aborted). */
export async function runReleasePlan(
  env: DeployEnv,
  items: ReleaseItem[],
  channel: ReleaseChannel,
  opts: RunReleaseOpts,
): Promise<number> {
  step("Reading release-state cursor");
  const cursor = await readReleaseCursor(env);

  step("Planning");
  const plans: PlanEntry[] = [];
  for (const item of items) {
    plans.push(await planItem(env, item, channel, cursor));
  }

  // Version-bump gate: any item whose source changed but whose version isn't
  // strictly greater than what's published is rejected (only when a prior
  // record exists; the first release of an item is always allowed). A
  // platform-fill change (sourceChanged === false) never requires a bump.
  const needsBump = plans.filter(
    (p) =>
      p.sourceChanged &&
      p.recordedVersion !== undefined &&
      !semverGt(p.localVersion, p.recordedVersion),
  );
  if (needsBump.length > 0) {
    console.log("\n" + colors.red(colors.bold("Release rejected: version bump required")) + "\n");
    for (const p of needsBump) {
      console.log(
        "  " +
          colors.red("✗") +
          ` ${p.item.label}: changed but still v${p.localVersion} ` +
          `(published v${p.recordedVersion}). Bump ${p.item.bumpHint}`,
      );
    }
    fail(`bump the version(s) above, then re-run.`);
  }

  const changed = plans.filter((p) => p.changed);
  if (changed.length === 0) {
    ok(`nothing to release; everything matches the ${channel} channel`);
    return 0;
  }

  console.log("\n" + colors.bold(`Release plan (${channel}):`) + "\n");
  for (const p of changed) {
    console.log("  " + colors.green("•") + ` ${p.item.label.padEnd(18)} ${p.desc}`);
  }
  console.log("");

  // Confirm before doing any work. Change detection already told us what
  // differs; the build happens as part of each item's apply() after the user
  // says yes.
  if (opts.dryRun) {
    info(colors.yellow("dry-run: building locally, no uploads or cursor write"));
  } else if (!opts.yes && !promptYesNo("Proceed with release?")) {
    info("aborted; nothing was released.");
    return 0;
  }

  // Each item's apply() appends the versioned keys it uploads to its own list so
  // the cursor below can record them and prune older versions. Items also append
  // the local files they want mirrored to the GitHub Release.
  //
  // Items are ISOLATED: one item's apply() throwing does not abort the rest. The
  // failure is collected, the remaining items still run, and the cursor + mirror +
  // bump below act only on the items that SUCCEEDED - so a re-run retries just the
  // failed items (change detection skips the already-published ones, including the
  // heavy driver builds). A run with any failure exits non-zero at the very end,
  // after the successes are safely recorded.
  const recordedKeys = new Map<string, string[]>();
  const releaseAssets: Array<{ path: string; name: string }> = [];
  const succeeded: PlanEntry[] = [];
  const failed: Array<{ item: ReleaseItem; error: Error }> = [];
  for (const p of changed) {
    step(`Releasing: ${p.item.label}`);
    const keys: string[] = [];
    try {
      await p.item.apply(env, channel, {
        prebuilt: opts.prebuilt,
        dryRun: opts.dryRun,
        recordVersionedKey: (k) => keys.push(k),
        recordReleaseAsset: (path, name) => releaseAssets.push({ path, name }),
      });
      recordedKeys.set(p.item.id, keys);
      succeeded.push(p);
    } catch (err) {
      const e = err instanceof Error ? err : new Error(String(err));
      info(colors.red(`✗ ${p.item.label} failed: ${e.message}`));
      failed.push({ item: p.item, error: e });
    }
  }

  const failSummary = () =>
    `${failed.length} item(s) failed: ${failed.map((f) => f.item.label).join(", ")}`;

  if (opts.dryRun) {
    ok(`dry-run complete; ${succeeded.length} item(s) built locally, nothing published`);
    if (failed.length > 0) fail(`${failSummary()} (dry-run build).`);
    return succeeded.length;
  }

  // Mirror only the SUCCEEDED items to the rolling per-channel GitHub Release
  // BEFORE writing the cursor, so a mirror failure fails the whole run and a
  // re-push retries (R2 puts are idempotent). Doing it after the cursor write
  // would let a mirror failure leave the cursor claiming success, so the re-push
  // would skip.
  if (opts.githubRelease && succeeded.length > 0) {
    await publishGithubRelease(env, channel, {
      items: succeeded.map((p) => ({ label: p.item.label, version: p.localVersion })),
      // Item-recorded assets plus any built outside the plan (the native Core
      // installers, uploaded by ci-publish before the plan runs).
      assets: [...releaseAssets, ...(opts.extraGithubAssets ?? [])],
    });
  }

  // No auto-bump: this never writes the repo. The cursor records the as-built
  // source hash, keeping a re-push of the same commit idempotent; the bump is a
  // separate committed step on `main` (ci-bump.ts).
  step("Updating release-state cursor");
  const pruneTargets: string[] = [];
  for (const p of succeeded) {
    const prev =
      p.item.scope === "shared" ? cursor.shared[p.item.id] : cursor.channels[channel]?.[p.item.id];
    // Record what was ACTUALLY released: the published version and its source
    // hash. Source hashes are version-BLIND (each item strips its version field
    // before hashing, see stripJsonVersion / stripCoreVersion), so the post-release
    // bump does NOT change an item's hash: the pending bump sits on `main` and
    // ships bundled with the next real source change (which does trip the hash),
    // rather than re-triggering a rebuild + republish on its own. A source change
    // WITHOUT a bump is still rejected by the version-bump gate above.
    const state: ItemState = {
      version: p.localVersion,
      sourceHash: p.localHash,
    };
    const keys = recordedKeys.get(p.item.id) ?? [];
    if (keys.length > 0) {
      // Versioned item: fold this version's keys into the retained history and
      // queue anything beyond the window for deletion.
      const merged = mergeVersionHistory(prev?.history, p.localVersion, keys, VERSION_RETENTION);
      state.history = merged.history;
      pruneTargets.push(...merged.evictedKeys);
    } else if (prev?.history) {
      // Latest-only item (or nothing uploaded this run): carry history forward.
      state.history = prev.history;
    }
    if (p.item.scope === "shared") {
      cursor.shared[p.item.id] = state;
    } else {
      (cursor.channels[channel] ??= {})[p.item.id] = state;
    }
  }
  // Write the cursor before pruning so a crash mid-prune leaves harmless orphans
  // rather than a cursor claiming a just-deleted version is retained.
  await writeReleaseCursor(env, cursor);
  ok(`cursor updated (${succeeded.length} item(s))`);

  if (pruneTargets.length > 0) {
    step(`Pruning ${pruneTargets.length} evicted artifact(s) from R2`);
    await mapPool(pruneTargets, 10, (key) => r2Delete(env, key));
  }

  // Successes are now published + recorded; surface the failures loudly and exit
  // non-zero so callers (and the branch-align step) treat the run as incomplete.
  if (failed.length > 0) {
    fail(
      `${failSummary()}. ${succeeded.length} published + recorded; fix and re-run to retry the rest.`,
    );
  }
  return succeeded.length;
}
