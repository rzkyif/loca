// Release item: loca-core (the Rust crate `loca-core-rs`) + the updater helper
// crate + the worker .ts files, composed into signed core.json and binaries.json
// and uploaded to R2. Versioned via CORE_VERSION in
// packages/loca-core-rs/src/config.rs. Extension tools run as sandboxed `deno`
// subprocesses, so `tool-worker.ts` ships beside the binary.

import { ensureDir } from "@std/fs/ensure-dir";
import { join } from "@std/path";
import type {
  BinaryKind,
  BinaryManifest,
  BinaryManifestEntry,
  BinaryManifestPinnedEntry,
  BinaryVariant,
  CoreManifest,
  PinnedTarget,
  Triple,
  UpstreamResolver,
} from "../../packages/loca-shared/src/domain/model.ts";
import {
  assetVariants,
  BINARY_KINDS,
  UPSTREAM_BINARIES,
} from "../../packages/loca-shared/src/domain/model.ts";
import {
  type ApplyOpts,
  bumpCoreVersion,
  channelBinSuffix,
  channelManifestDir,
  channelStoragePrefix,
  colors,
  CORE_CONFIG_RS,
  type DeployEnv,
  detectHostTriple,
  DIST_DIR,
  fail,
  hashPaths,
  humanBytes,
  info,
  ok,
  packagesHashInputs,
  mapPool,
  R2_CONCURRENCY,
  r2Put,
  readCoreVersion,
  rel,
  type ReleaseChannel,
  type ReleaseItem,
  REPO_ROOT,
  sha256File,
  signEd25519,
  step,
  stripCoreVersion,
} from "./lib.ts";
import { mergeCoreBundles } from "./artifacts.ts";

// ---------------------------------------------------------------------------
// constants

const ALL_TRIPLES: Triple[] = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "aarch64-unknown-linux-gnu",
  "x86_64-unknown-linux-gnu",
  "x86_64-pc-windows-msvc",
  "aarch64-pc-windows-msvc",
];

// Worker .ts files shipped alongside the binary (platform-independent).
// These are NOT bundled into the core binary. At runtime, the core spawns
// them as Deno subprocesses from ~/.loca/core/workers/.
const WORKER_FILES = ["tool-worker.ts"] as const;

// One helper. The updater is a SEPARATE BINARY on purpose: it swaps the core
// executable during self-update, and a process cannot replace the file it is
// currently running from on Windows.
const HELPER_CRATES: Array<{
  name: "loca-core-updater";
  crateDir: string;
}> = [{ name: "loca-core-updater", crateDir: "packages/loca-core-updater" }];

const MANIFEST_CACHE_CONTROL = "public, max-age=300";

// ---------------------------------------------------------------------------
// types

export interface BuildArtifact {
  triple: Triple;
  name: "loca-core";
  filename: string;
  path: string;
  sha256: string;
  size: number;
}

export interface WorkerArtifact {
  name: string;
  path: string;
  sha256: string;
  size: number;
}

export interface HelperArtifact {
  triple: Triple;
  entryName: string;
  filename: string;
  path: string;
  sha256: string;
  size: number;
}

// Everything a single environment builds for the core release item. The host
// concatenates one of these per build environment, then composes + signs +
// uploads the unified manifests once over the union (composeAndUploadCore).
export interface CoreBuildArtifacts {
  artifacts: BuildArtifact[];
  helpers: HelperArtifact[];
  workers: WorkerArtifact[];
}

// ---------------------------------------------------------------------------
// build

/// The Rust core crate, and the one binary target it produces.
const CORE_CRATE_DIR = "packages/loca-core-rs";
const CORE_BIN = "loca-core";

/// Features a SHIPPING core is built with. Both are off by default in the crate
/// so a fresh checkout with no `vendor/` still builds, but a release must have
/// them: without `local-inference` there is no on-device model at all, and
/// without `local-speech` local STT/TTS silently falls back to external
/// providers. Neither is a flag.
const CORE_FEATURES = ["local-inference", "local-speech"] as const;

/// llama.cpp is vendored by script rather than by submodule, and `vendor/` is
/// gitignored, so a release host's fresh checkout does not have it. Idempotent
/// and cheap when already at the pin; run rather than assumed, because a missing
/// tree otherwise surfaces as a compiler error.
///
/// **espeak-ng-data is deliberately NOT fetched here.** Its archive is COMMITTED
/// and pulled in with `include_bytes!`; `scripts/vendor-espeak.ts` is a
/// maintenance tool, not a build step.
async function ensureVendoredSources(): Promise<void> {
  info("vendoring llama.cpp at the pinned SHA");
  const { code } = await new Deno.Command(Deno.execPath(), {
    args: ["run", "-A", join(REPO_ROOT, "scripts", "vendor-llama.ts")],
    cwd: REPO_ROOT,
    stdout: "inherit",
    stderr: "inherit",
  }).output();
  if (code !== 0) fail(`vendor-llama.ts exited ${code}`);
}

/// Copy the llama.cpp runtime libraries that must sit BESIDE the core binary.
///
/// `GGML_BACKEND_DL` makes every backend a separately loaded shared library,
/// **including the CPU one**: with none present the process registers zero
/// compute devices and cannot run a model. `backends::directories` looks for
/// them in the install directory (wherever the binary is unpacked).
///
/// Returns the filenames copied, so the caller can fail on an empty result.
async function copyRuntimeLibs(builtDir: string, outDir: string): Promise<string[]> {
  const copied: string[] = [];
  for await (const entry of Deno.readDir(builtDir)) {
    if (!entry.isFile) continue;
    const name = entry.name;
    // The same predicate `build.rs` stages with, deliberately: a file it staged
    // and this skipped would be a core that builds and cannot load a backend.
    if (!(name.includes(".dylib") || name.includes(".so") || name.endsWith(".dll"))) continue;
    await Deno.copyFile(join(builtDir, name), join(outDir, name));
    copied.push(name);
  }
  return copied;
}

export async function buildAll(triples: Triple[], suffix: string): Promise<BuildArtifact[]> {
  await ensureVendoredSources();
  await ensureRustTargets(triples);

  const artifacts: BuildArtifact[] = [];
  for (const triple of triples) {
    const isWin = triple.includes("windows");
    const exe = isWin ? ".exe" : "";
    const name = CORE_BIN;
    const filename = `${name}${suffix}${exe}`;
    const outDir = join(DIST_DIR, triple);
    await ensureDir(outDir);
    const outPath = join(outDir, filename);

    info(`cargo build ${name}${suffix} for ${triple}`);
    const { code } = await new Deno.Command("cargo", {
      args: [
        "build",
        "--release",
        // The same supply-chain guarantee the helpers already had: the signed
        // artifact embeds the dependency closure reviewed at this commit rather
        // than whatever in-range patch is in the builder's cache.
        "--locked",
        "--manifest-path",
        join(REPO_ROOT, CORE_CRATE_DIR, "Cargo.toml"),
        "--bin",
        CORE_BIN,
        "--features",
        CORE_FEATURES.join(","),
        "--target",
        triple,
      ],
      stdout: "inherit",
      stderr: "inherit",
    }).output();
    if (code !== 0) fail(`cargo build ${name} for ${triple} exited ${code}`);

    const builtDir = join(cargoTargetDir(), triple, "release");
    await Deno.copyFile(join(builtDir, `${name}${exe}`), outPath);

    const libs = await copyRuntimeLibs(builtDir, outDir);
    if (libs.length === 0) {
      fail(
        `no llama.cpp runtime libraries were staged beside ${filename} for ${triple}. ` +
          `Every backend is a loadable module under GGML_BACKEND_DL, the CPU one included, ` +
          `so this core would register zero compute devices and run no model.`,
      );
    }
    info(`staged ${libs.length} runtime librar${libs.length === 1 ? "y" : "ies"} for ${triple}`);

    const { sha256, size } = await sha256File(outPath);
    artifacts.push({ triple, name, filename, path: outPath, sha256, size });
  }
  return artifacts;
}

// Where cargo writes its build output. Honors CARGO_TARGET_DIR when a build
// environment sets it; falls back to the in-repo target/, where the env is unset.
function cargoTargetDir(): string {
  return Deno.env.get("CARGO_TARGET_DIR") ?? join(REPO_ROOT, "target");
}

// `cargo --target <triple>` needs that target's std installed. Add each
// idempotently via rustup first (a no-op when present); best-effort, so an
// absent rustup falls through to cargo's own clearer error.
async function ensureRustTargets(triples: Triple[]): Promise<void> {
  for (const triple of triples) {
    const code = await new Deno.Command("rustup", {
      args: ["target", "add", triple],
      stdout: "inherit",
      stderr: "inherit",
    })
      .output()
      .then((o) => o.code)
      .catch(() => 1);
    if (code !== 0) {
      info(colors.yellow(`rustup target add ${triple} skipped (rustup absent or add failed)`));
    }
  }
}

export async function buildHelpers(triples: Triple[], suffix: string): Promise<HelperArtifact[]> {
  const out: HelperArtifact[] = [];
  for (const triple of triples) {
    const isWin = triple.includes("windows");
    const exe = isWin ? ".exe" : "";
    for (const { name, crateDir } of HELPER_CRATES) {
      // Cargo always builds the bare crate name; we copy it out under the
      // channel-suffixed name so latest's loca-core-updater-latest coexists.
      const entryName = `${name}${suffix}`;
      const filename = `${entryName}${exe}`;
      const outDir = join(DIST_DIR, triple);
      await ensureDir(outDir);
      const outPath = join(outDir, filename);
      info(`cargo build ${name} for ${triple}`);
      const cmd = new Deno.Command("cargo", {
        args: [
          "build",
          "--release",
          // Build the signed helper from the committed, CI-verified Cargo.lock
          // (never a re-resolved graph), so the artifact's dependency closure is
          // exactly the one reviewed at this commit.
          "--locked",
          "--manifest-path",
          join(REPO_ROOT, crateDir, "Cargo.toml"),
          "--target",
          triple,
        ],
        stdout: "inherit",
        stderr: "inherit",
      });
      const { code } = await cmd.output();
      if (code !== 0) fail(`cargo build ${name} for ${triple} exited ${code}`);
      const builtPath = join(cargoTargetDir(), triple, "release", `${name}${exe}`);
      await Deno.copyFile(builtPath, outPath);
      const { sha256, size } = await sha256File(outPath);
      out.push({ triple, entryName, filename, path: outPath, sha256, size });
    }
  }
  return out;
}

export async function hashWorkers(): Promise<WorkerArtifact[]> {
  const workersDir = join(REPO_ROOT, "packages/loca-core-rs/workers");
  const out: WorkerArtifact[] = [];
  for (const name of WORKER_FILES) {
    const path = join(workersDir, name);
    const { sha256, size } = await sha256File(path);
    out.push({ name, path, sha256, size });
  }
  return out;
}

// ---------------------------------------------------------------------------
// manifest assembly

async function composeCoreManifest(
  version: string,
  artifacts: BuildArtifact[],
  workers: WorkerArtifact[],
  helpers: HelperArtifact[],
  storageDomain: string,
  storagePrefix: string,
  privateKey: Uint8Array,
): Promise<CoreManifest> {
  // URLs point at the gzip-compressed artifacts (`.gz`); `sha256` is over the
  // DECOMPRESSED file, so consumers (self-install + self-updater) gunzip the
  // download then verify the same hash against the real binary/script.
  const binaries = artifacts
    .filter((a) => a.name === "loca-core")
    .map((a) => ({
      triple: a.triple,
      url: `https://${storageDomain}/${storagePrefix}${version}/${a.triple}/${a.filename}.gz`,
      sha256: a.sha256,
    }));

  const workerEntries = workers.map((w) => ({
    name: w.name,
    url: `https://${storageDomain}/${storagePrefix}${version}/workers/${w.name}.gz`,
    sha256: w.sha256,
  }));

  const helperEntries = helpers.map((h) => ({
    name: h.entryName,
    triple: h.triple,
    url: `https://${storageDomain}/${storagePrefix}${version}/${h.triple}/${h.filename}.gz`,
    sha256: h.sha256,
  }));

  // Sign the WHOLE manifest minus `signature`: the core's verifier strips the
  // same field, so binaries, workers and helpers are all authenticated.
  const manifest: Omit<CoreManifest, "signature"> = {
    schemaVersion: 1,
    version,
    binaries,
    workers: workerEntries,
    helpers: helperEntries,
  };
  const signature = await signEd25519(privateKey, manifest);
  return { ...manifest, signature };
}

interface GitHubReleaseAsset {
  name: string;
  browser_download_url: string;
  digest?: string | null;
  /** GitHub marks an asset "uploaded" only once fully stored; other states are
   *  in-progress uploads we must not pin. */
  state?: string;
}

interface GitHubRelease {
  tag_name: string;
  assets: GitHubReleaseAsset[];
  draft?: boolean;
  prerelease?: boolean;
}

async function githubJson(url: string): Promise<unknown> {
  const headers: Record<string, string> = {
    accept: "application/vnd.github+json",
    "user-agent": "loca-release",
  };
  const ghToken = Deno.env.get("GITHUB_TOKEN");
  if (ghToken) headers.authorization = `Bearer ${ghToken}`;
  const res = await fetch(url, { headers });
  if (!res.ok) {
    fail(`GitHub API ${res.status} for ${url}: ${await res.text().catch(() => "")}`);
  }
  return await res.json();
}

/** True when `assets` carries a fully-uploaded asset for every triple's primary
 *  cpu build (the guaranteed fallback we require to pin a triple). Used to skip a
 *  just-published release whose asset matrix is still uploading. */
function releaseHasCpuAssets(release: GitHubRelease, assets: UpstreamResolver["assets"]): boolean {
  for (const [, tripleAsset] of Object.entries(assets)) {
    const cpu = assetVariants(tripleAsset).cpu;
    if (!cpu) continue;
    const name = cpu.asset.replace(/\{tag\}/g, release.tag_name);
    const a = release.assets.find((x) => x.name === name);
    if (!a || (a.state !== undefined && a.state !== "uploaded")) return false;
  }
  return true;
}

/** The release to pin for a resolver. A `pinnedTag` yields exactly that release;
 *  otherwise the newest recent release whose per-triple cpu assets are fully
 *  uploaded (upstream CI publishes the tag before its asset matrix finishes
 *  uploading, so `/releases/latest` can point at an incomplete release). */
async function selectRelease(
  repo: string,
  assets: UpstreamResolver["assets"],
  pinnedTag?: string,
): Promise<GitHubRelease> {
  if (pinnedTag) {
    return (await githubJson(
      `https://api.github.com/repos/${repo}/releases/tags/${pinnedTag}`,
    )) as GitHubRelease;
  }
  const list = (await githubJson(
    `https://api.github.com/repos/${repo}/releases?per_page=10`,
  )) as GitHubRelease[];
  // Exclude drafts + prereleases (the list endpoint includes them, unlike
  // `/releases/latest`), then take the newest with a complete cpu asset set.
  const complete = list.find((r) => !r.draft && !r.prerelease && releaseHasCpuAssets(r, assets));
  if (!complete) {
    fail(`no recent ${repo} release has a complete cpu asset set (still uploading?)`);
  }
  return complete;
}

async function sha256OfUrl(url: string): Promise<string> {
  const res = await fetch(url);
  if (!res.ok || !res.body) {
    fail(`could not fetch ${url} for SHA-256: ${res.status} ${res.statusText}`);
  }
  const reader = res.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    if (value) {
      chunks.push(value);
      total += value.byteLength;
    }
  }
  const buf = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    buf.set(c, off);
    off += c.byteLength;
  }
  const hash = await crypto.subtle.digest("SHA-256", buf);
  const hex = Array.from(new Uint8Array(hash))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  return hex;
}

async function composeBinaryManifest(
  privateKey: Uint8Array,
  channel: ReleaseChannel,
  version: string,
): Promise<BinaryManifest> {
  const binaries = {} as Record<BinaryKind, BinaryManifestEntry>;
  for (const kind of BINARY_KINDS) {
    const resolver = UPSTREAM_BINARIES[kind];
    if (!resolver) continue;

    // Latest: ship the resolver itself (repo + asset patterns + optional pinned
    // tag) so the core resolves it at runtime. No GitHub call here: the signed
    // manifest commits to the repo/patterns (and the pin, when declared).
    if (channel === "latest") {
      info(`latest resolver entry for ${kind} → ${resolver.repo}`);
      binaries[kind] = {
        resolver: {
          repo: resolver.repo,
          assets: resolver.assets,
          ...(resolver.pinnedTag ? { pinnedTag: resolver.pinnedTag } : {}),
        },
      };
      continue;
    }

    // Stable: pin URL + sha256 at release time, from the declared pinned tag
    // when one exists (deno) or the current latest otherwise. Each triple
    // resolves a per-variant map (cpu plus any GPU builds); `cpu` is required
    // (the guaranteed fallback), GPU variants absent from this tag are skipped.
    info(`resolving ${resolver.pinnedTag ?? "latest"} ${kind} from ${resolver.repo}`);
    const release = await selectRelease(resolver.repo, resolver.assets, resolver.pinnedTag);
    const tag = release.tag_name;
    info(`  tag: ${tag}`);

    // Resolve one asset-name pattern to a pinned {url, sha256}, or null when the
    // named asset is absent from this release.
    const resolveOne = async (pattern: string): Promise<{ url: string; sha256: string } | null> => {
      const assetName = pattern.replace(/\{tag\}/g, tag);
      const asset = release.assets.find((a) => a.name === assetName);
      // Absent, or present but its upload hasn't finished: treat as unavailable.
      if (!asset || (asset.state !== undefined && asset.state !== "uploaded")) return null;
      let sha256: string;
      if (asset.digest && asset.digest.startsWith("sha256:")) {
        sha256 = asset.digest.slice("sha256:".length);
      } else {
        info(`  hashing ${assetName}`);
        sha256 = await sha256OfUrl(asset.browser_download_url);
      }
      return { url: asset.browser_download_url, sha256 };
    };

    const platforms = {} as BinaryManifestPinnedEntry["platforms"];
    for (const [triple, tripleAsset] of Object.entries(resolver.assets)) {
      const resolvedVariants: Partial<Record<BinaryVariant, PinnedTarget>> = {};
      for (const [variant, va] of Object.entries(assetVariants(tripleAsset))) {
        const primary = await resolveOne(va.asset);
        if (!primary) {
          info(
            colors.yellow(
              `  no asset "${va.asset.replace(/\{tag\}/g, tag)}" in ${resolver.repo}@${tag}; ` +
                `skipping ${variant} for ${triple}`,
            ),
          );
          continue;
        }
        // A GPU variant needs all its companion archives (e.g. cudart); if one
        // is missing, drop the whole variant so we never pin a half-set.
        const extras: { url: string; sha256: string }[] = [];
        let extrasOk = true;
        for (const extraPattern of va.extra ?? []) {
          const e = await resolveOne(extraPattern);
          if (!e) {
            info(colors.yellow(`  missing extra "${extraPattern}" for ${variant}/${triple}; skip`));
            extrasOk = false;
            break;
          }
          extras.push(e);
        }
        if (!extrasOk) continue;
        resolvedVariants[variant as BinaryVariant] = {
          url: primary.url,
          sha256: primary.sha256,
          ...(extras.length ? { extras } : {}),
        };
      }
      const keys = Object.keys(resolvedVariants);
      if (!resolvedVariants.cpu) {
        info(
          colors.yellow(`  no cpu variant resolved for ${kind} triple ${triple}; skipping triple`),
        );
        continue;
      }
      // Collapse a cpu-only triple to the bare pinned shape (matches single-variant
      // kinds and keeps the manifest tidy); otherwise emit the per-variant map.
      platforms[triple as Triple] = keys.length === 1 ? resolvedVariants.cpu : resolvedVariants;
    }
    if (Object.keys(platforms).length === 0) {
      fail(`no platforms resolved for ${kind} (check asset name patterns)`);
    }
    binaries[kind] = { version: tag, platforms };
  }

  // Sign the WHOLE manifest minus `signature` (matching core.json/extension.json),
  // so the monotonic `version` the runtime uses for downgrade refusal is
  // authenticated, not just `binaries`.
  const unsigned: Omit<BinaryManifest, "signature"> = {
    schemaVersion: 1,
    version,
    binaries,
  };
  const signature = await signEd25519(privateKey, unsigned);
  return { ...unsigned, signature };
}

/** Gzip a file to `<srcPath>.gz` for transport. The core's single-file
 *  artifacts (binary, helpers, worker .ts) ship gzip-compressed; the manifest
 *  sha256 stays over the DECOMPRESSED file, so consumers gunzip then verify the
 *  same hash. Mirrors extension.ts's `CompressionStream("gzip")` packing. */
async function gzipFile(
  srcPath: string,
  outPath?: string,
): Promise<{ path: string; size: number }> {
  // Default alongside the source (dist artifacts, already gitignored); callers
  // whose source lives in the repo tree (e.g. the worker .ts) pass an explicit
  // dist path so the .gz never lands in tracked source.
  const gzPath = outPath ?? `${srcPath}.gz`;
  const out = await Deno.open(gzPath, {
    create: true,
    write: true,
    truncate: true,
  });
  await (
    await Deno.open(srcPath)
  ).readable
    .pipeThrough(new CompressionStream("gzip"))
    .pipeTo(out.writable);
  const { size } = await Deno.stat(gzPath);
  return { path: gzPath, size };
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

// Packages composed into the core release item. CORE_HASH_INPUTS derives from
// this list, so adding a helper crate cannot silently skip the source-hash gate.
// Lockfiles are excluded (a client-only dep bump touches the root lock but not
// core); the declared deps in each manifest ARE hashed.
const CORE_PACKAGES = ["core-rs", "shared", "core-updater"];
// Files of the Rust crate outside `src/` that change the shipped artifact.
const CORE_HASH_INPUTS = [
  ...packagesHashInputs(CORE_PACKAGES),
  "packages/loca-core-rs/build.rs",
  "packages/loca-core-rs/schema.sql",
  "packages/loca-core-rs/workers",
];

// The dist/<triple>/ binaries buildAll + buildHelpers emit for this channel:
// loca-core plus the updater helper, each with the channel suffix. `deno task
// build` hash-checks these so a wiped or swapped artifact forces a rebuild.
// Always the host triple: every build (local or a CI runner) is native.
export function coreOutputs(channel: ReleaseChannel): string[] {
  const triples = [detectHostTriple()];
  const suffix = channelBinSuffix(channel);
  const names = ["loca-core", ...HELPER_CRATES.map((h) => h.name)];
  return triples.flatMap((triple) => {
    const exe = triple.includes("windows") ? ".exe" : "";
    return names.map((n) => join(DIST_DIR, triple, `${n}${suffix}${exe}`));
  });
}

export const coreItem: ReleaseItem = {
  id: "core",
  label: "core + helpers",
  scope: "channel",
  packages: CORE_PACKAGES,
  bumpHint: "packages/loca-core-rs/src/config.rs (CORE_VERSION)",

  version: readCoreVersion,
  versionFile: CORE_CONFIG_RS,
  bumpVersion: bumpCoreVersion,

  sourceHash(_channel: ReleaseChannel): Promise<string> {
    // config.rs carries CORE_VERSION; hash it with the version blanked so a lone
    // bump does not re-trigger a core release.
    const rustConfigRel = rel(CORE_CONFIG_RS);
    return hashPaths([
      ...CORE_HASH_INPUTS.map((p) => ({
        path: join(REPO_ROOT, p),
        exclude: (r: string) => r.endsWith(".test.ts") || r === rustConfigRel,
      })),
      { path: CORE_CONFIG_RS, transform: stripCoreVersion },
    ]);
  },

  buildOutputs(channel: ReleaseChannel): Promise<string[]> {
    return Promise.resolve(coreOutputs(channel));
  },

  async apply(env: DeployEnv, channel: ReleaseChannel, opts: ApplyOpts): Promise<void> {
    const version = await readCoreVersion();
    // Merge the bundles the build runners produced (their dist/<triple>/ files
    // already collected under DIST_DIR) and hash the platform-independent
    // workers from this coordinator's checkout. Nothing is built here.
    const built = await mergeCoreBundles(opts.prebuilt.coreBundles, await hashWorkers());
    await composeAndUploadCore(env, channel, version, built, opts);
    // Native installers are NOT built here: they belong to the client item
    // (client.ts), which wraps the just-published core artifacts and its own
    // bundle into the host OS's installers. In CI each build runner packages
    // its own OS's installers (ci-build.ts).
  },
};

// Build everything the core release item needs for a set of triples: the core
// binaries, the native helpers, and the platform-independent worker hashes. Runs
// on each build runner; composeAndUploadCore aggregates the results.
export async function buildCoreArtifacts(
  triples: Triple[],
  suffix: string,
): Promise<CoreBuildArtifacts> {
  step(`Building core binaries (${triples.length} triples)`);
  const artifacts = await buildAll(triples, suffix);
  for (const a of artifacts) {
    ok(`${a.triple}/${a.filename}  ${humanBytes(a.size)}  ${a.sha256.slice(0, 12)}…`);
  }

  step(`Building native helpers (${triples.length} triples)`);
  await ensureRustTargets(triples);
  const helpers = await buildHelpers(triples, suffix);
  for (const h of helpers) {
    ok(`${h.triple}/${h.filename}  ${humanBytes(h.size)}  ${h.sha256.slice(0, 12)}…`);
  }

  step("Hashing worker scripts");
  const workers = await hashWorkers();
  for (const w of workers) {
    ok(`workers/${w.name}  ${humanBytes(w.size)}  ${w.sha256.slice(0, 12)}…`);
  }

  return { artifacts, helpers, workers };
}

// Compose + sign core.json/binaries.json over the given artifact set, then
// upload artifacts + manifests to R2. Runs ONCE on the publish coordinator: the
// composers carry nothing forward from R2, so the arrays passed here must
// already be the full union across every triple the matrix staged.
export async function composeAndUploadCore(
  env: DeployEnv,
  channel: ReleaseChannel,
  version: string,
  built: CoreBuildArtifacts,
  opts: ApplyOpts,
): Promise<void> {
  const { artifacts, helpers, workers } = built;
  const prefix = channelStoragePrefix(channel);
  const manifestDir = channelManifestDir(channel);

  step("Composing + signing core.json");
  const coreManifest = await composeCoreManifest(
    version,
    artifacts,
    workers,
    helpers,
    env.storageDomain,
    prefix,
    env.signingPrivateKey,
  );
  const coreJsonPath = await writeManifestFile(manifestDir, "core.json", coreManifest);
  ok(`signed core.json → ${rel(coreJsonPath)}`);

  step("Composing + signing binaries.json");
  const binaryManifest = await composeBinaryManifest(env.signingPrivateKey, channel, version);
  const binJsonPath = await writeManifestFile(manifestDir, "binaries.json", binaryManifest);
  ok(`signed binaries.json → ${rel(binJsonPath)}`);

  if (opts.dryRun) {
    info(colors.yellow(`dry-run: manifests under ${rel(join(DIST_DIR, manifestDir))}, no upload`));
    return;
  }

  // Single-file artifacts ship gzip-compressed (see gzipFile); the manifest
  // URLs end in `.gz` and consumers verify sha256 over the decompressed file.
  step(`Uploading binaries to R2 bucket "${env.r2Bucket}"`);
  await mapPool(artifacts, R2_CONCURRENCY, async (a) => {
    const gz = await gzipFile(a.path);
    const key = `${prefix}${version}/${a.triple}/${a.filename}.gz`;
    info(`uploading ${key}  (${humanBytes(gz.size)}, raw ${humanBytes(a.size)})`);
    await r2Put(env, key, gz.path, "application/gzip");
    opts.recordVersionedKey?.(key);
    opts.recordReleaseAsset?.(gz.path, `${a.triple}_${a.filename}.gz`);
  });
  ok(`uploaded ${artifacts.length} binaries`);

  step(`Uploading helpers to R2 bucket "${env.r2Bucket}"`);
  await mapPool(helpers, R2_CONCURRENCY, async (h) => {
    const gz = await gzipFile(h.path);
    const key = `${prefix}${version}/${h.triple}/${h.filename}.gz`;
    info(`uploading ${key}  (${humanBytes(gz.size)}, raw ${humanBytes(h.size)})`);
    await r2Put(env, key, gz.path, "application/gzip");
    opts.recordVersionedKey?.(key);
    opts.recordReleaseAsset?.(gz.path, `${h.triple}_${h.filename}.gz`);
  });
  ok(`uploaded ${helpers.length} helpers`);

  step(`Uploading workers to R2 bucket "${env.r2Bucket}"`);
  // Worker sources live in the repo tree, so gzip them into dist/ (gitignored)
  // rather than next to the .ts, which would leave an untracked artifact.
  const workersOut = join(DIST_DIR, "workers");
  await ensureDir(workersOut);
  await mapPool(workers, R2_CONCURRENCY, async (w) => {
    const gz = await gzipFile(w.path, join(workersOut, `${w.name}.gz`));
    const key = `${prefix}${version}/workers/${w.name}.gz`;
    info(`uploading ${key}  (${humanBytes(gz.size)}, raw ${humanBytes(w.size)})`);
    await r2Put(env, key, gz.path, "application/gzip");
    opts.recordVersionedKey?.(key);
  });
  ok(`uploaded ${workers.length} workers`);

  step(`Uploading manifests to R2 bucket "${env.r2Bucket}"`);
  await r2Put(
    env,
    `${manifestDir}/core.json`,
    coreJsonPath,
    "application/json",
    MANIFEST_CACHE_CONTROL,
  );
  ok(`uploaded ${manifestDir}/core.json`);
  await r2Put(
    env,
    `${manifestDir}/binaries.json`,
    binJsonPath,
    "application/json",
    MANIFEST_CACHE_CONTROL,
  );
  ok(`uploaded ${manifestDir}/binaries.json`);
}

export { ALL_TRIPLES };
