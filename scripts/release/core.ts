// Release item: tomat-core (deno-compiled) + the four native helper crates
// (updater, keychain, hwinfo, ptyhost) + the worker .ts files, composed into
// signed core.json and binaries.json and uploaded to R2. Versioned via
// CORE_VERSION in packages/tomat-core/src/config.ts.

import { copy } from "@std/fs/copy";
import { ensureDir } from "@std/fs/ensure-dir";
import { dirname, join } from "@std/path";
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
} from "../../packages/tomat-shared/src/domain/model.ts";
import {
  assetVariants,
  BINARY_KINDS,
  binaryUnavailableOnTriple,
  UPSTREAM_BINARIES,
} from "../../packages/tomat-shared/src/domain/model.ts";
import {
  type ApplyOpts,
  bumpCoreVersion,
  channelBinSuffix,
  channelManifestDir,
  channelStoragePrefix,
  colors,
  CONFIG_PATH,
  CORE_DIR,
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
import { reportRouting, routeTriples } from "./all-targets.ts";
import { type ArtifactBundle, mergeCoreBundles } from "./artifacts.ts";
import { withEnvironment } from "./drivers/lifecycle.ts";
import type { BuildEnvironment } from "./drivers/mod.ts";

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
// them as Deno subprocesses from ~/.tomat/core/workers/.
const WORKER_FILES = ["tool-worker.ts"] as const;

const HELPER_CRATES: Array<{
  name: "tomat-core-keychain" | "tomat-core-updater" | "tomat-core-hwinfo" | "tomat-core-ptyhost";
  crateDir: string;
}> = [
  { name: "tomat-core-keychain", crateDir: "packages/tomat-core-keychain" },
  { name: "tomat-core-updater", crateDir: "packages/tomat-core-updater" },
  { name: "tomat-core-hwinfo", crateDir: "packages/tomat-core-hwinfo" },
  { name: "tomat-core-ptyhost", crateDir: "packages/tomat-core-ptyhost" },
];

// tomat-core-speech is OURS but, unlike the eager helpers, it's consent-gated +
// on-demand: built host-only here, packaged with its espeak-ng-data, and pinned
// into binaries.json (not core.json) so it downloads only when STT/TTS is on.
const SPEECH_CRATE = {
  name: "tomat-core-speech",
  crateDir: "packages/tomat-core-speech",
} as const;

// Canonical standalone espeak-ng-data (the Kokoro phonemizer data the sherpa
// static lib needs at runtime but doesn't bundle). bzip2, so unpacked via the
// system `tar`, then repacked into the speech binary's gzip archive.
const ESPEAK_DATA_URL =
  "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/espeak-ng-data.tar.bz2";

// Pinned sha256 of the espeak-ng-data archive. The URL above is a MUTABLE rolling
// tag, and these bytes get baked into the tomat-signed speech binary, so the
// archive is verified against this hash before unpacking. If upstream legitimately
// republishes the asset, recompute and update this constant in the same commit.
const ESPEAK_DATA_SHA256 = "4135ccf82e1f40613491c0874d4945ae9e9c7840933d8e25a6f9e003d9ebf533";

const MANIFEST_CACHE_CONTROL = "public, max-age=300";

// ---------------------------------------------------------------------------
// types

export interface BuildArtifact {
  triple: Triple;
  name: "tomat-core";
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

export interface SpeechArtifact {
  triple: Triple;
  // The GPU build variant this archive is. `cpu` is the statically-linked
  // default (always built); GPU variants are built only when a prebuilt GPU
  // sherpa-onnx lib dir is staged (see buildSpeech / SPEECH_GPU_VARIANTS).
  variant: BinaryVariant;
  // `<name>[-<variant>]<suffix>.tar.gz`. The sha256 is over this ARCHIVE
  // (verified before extraction), unlike the single-file `.gz` artifacts whose
  // sha is over the decompressed file.
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
  speech: SpeechArtifact[];
  workers: WorkerArtifact[];
}

// ---------------------------------------------------------------------------
// build

// Packages in the core's module graph; only these are copied into the compile
// workspace. `tomat-core` imports `@tomat/shared`, `@tomat/core-engine`, and
// `@tomat/model-catalog` via relative paths, so all must be present for
// resolution to stay inside the temp tree.
const COMPILE_WORKSPACE_PACKAGES = [
  "tomat-shared",
  "tomat-core-engine",
  "tomat-core",
  "tomat-model-catalog",
];

/** Isolated workspace for `deno compile`. `deno compile` embeds every npm dep
 *  recorded in the deno.lock plus every entry in the compiled package's import
 *  map -- not only what the entry actually imports. The repo's lock and import
 *  maps span the whole workspace (client + website deps), so compiling against
 *  them bakes hundreds of MB of unused packages (workerd, onnxruntime-web, ...)
 *  into the binary.
 *
 *  To avoid that we COPY (not symlink, so resolution cannot escape back to the
 *  repo root and its workspace-wide lock + node_modules) only the packages in
 *  the core's graph, then generate a lock scoped to just those packages. The
 *  result embeds only the core's own graph (~100 MB). Worker ML deps stay out
 *  of `tomat-core`'s import map -- the workers carry their own `npm:` specifiers
 *  -- so they are never pulled into this scope. */
async function setupCompileWorkspace(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "tomat-compile-" });
  await ensureDir(join(dir, "packages"));
  for (const pkg of COMPILE_WORKSPACE_PACKAGES) {
    await copy(join(REPO_ROOT, "packages", pkg), join(dir, "packages", pkg));
    // A stray per-package node_modules would be embedded wholesale; drop it.
    await Deno.remove(join(dir, "packages", pkg, "node_modules"), {
      recursive: true,
    }).catch(() => {});
  }
  await Deno.writeTextFile(
    join(dir, "deno.json"),
    JSON.stringify(
      {
        workspace: COMPILE_WORKSPACE_PACKAGES.map((p) => `./packages/${p}`),
        unstable: ["raw-imports"],
      },
      null,
      2,
    ),
  );
  // Seed the workspace with the committed, CI-verified root lock so `deno install`
  // REUSES its exact versions + integrity hashes for every dep it already pins
  // (which is all of the core's graph, since the root lock spans the whole
  // workspace) instead of re-resolving each `^` range from the build cache. This
  // is the supply-chain guarantee the signed Rust helpers already get via
  // `--locked`: the compiled, Ed25519-signed Core embeds the dependency closure
  // reviewed at this commit, not whatever newer in-range patch happens to be in
  // the builder's cache. We do NOT pass `--frozen`: the root lock also pins the
  // client/website-only deps, and the scoped 4-package workspace must PRUNE those,
  // which `--frozen` would reject as drift. A plain `deno install` reuses the
  // pinned entries and prunes the unused ones, keeping both the integrity pin and
  // the scoped (~100 MB) graph.
  await Deno.copyFile(join(REPO_ROOT, "deno.lock"), join(dir, "deno.lock"));
  const { code } = await new Deno.Command("deno", {
    args: ["install"],
    cwd: dir,
    stdout: "inherit",
    stderr: "inherit",
  }).output();
  if (code !== 0) fail(`deno install in compile workspace exited ${code}`);
  return dir;
}

async function compileFor(
  triple: Triple,
  name: string,
  entryRelative: string,
  compileWorkspace: string,
): Promise<string> {
  const isWin = triple.includes("windows");
  const exe = isWin ? ".exe" : "";
  const outDir = join(DIST_DIR, triple);
  await ensureDir(outDir);
  const outPath = join(outDir, `${name}${exe}`);
  // `deno compile` can cross-compile to every triple EXCEPT aarch64-pc-windows-msvc
  // (it's absent from --target's list). It CAN compile that triple natively, so
  // there we drop --target and require the running deno to already be the
  // arm64-windows build (the Windows driver installs it). Every other triple
  // cross-compiles via --target.
  const nativeOnly = triple === "aarch64-pc-windows-msvc";
  if (nativeOnly && Deno.build.target !== triple) {
    fail(
      `building ${triple} needs the arm64-windows deno on PATH (deno compile can't ` +
        `cross-target it); the running deno is ${Deno.build.target}.`,
    );
  }
  const targetArgs = nativeOnly ? [] : ["--target", triple];
  const cmd = new Deno.Command("deno", {
    args: ["compile", "--allow-all", ...targetArgs, "--output", outPath, entryRelative],
    cwd: compileWorkspace,
    stdout: "inherit",
    stderr: "inherit",
  });
  const { code } = await cmd.output();
  if (code !== 0) fail(`deno compile ${name} for ${triple} exited ${code}`);
  return outPath;
}

export async function buildAll(triples: Triple[], suffix: string): Promise<BuildArtifact[]> {
  let compileWorkspace: string | null = null;
  try {
    const artifacts: BuildArtifact[] = [];
    for (const triple of triples) {
      const isWin = triple.includes("windows");
      const exe = isWin ? ".exe" : "";
      for (const [name, entryRelative] of [
        ["tomat-core", "packages/tomat-core/src/main.ts"],
      ] as const) {
        const filename = `${name}${suffix}${exe}`;
        const outPath = join(DIST_DIR, triple, filename);
        if (!compileWorkspace) {
          compileWorkspace = await setupCompileWorkspace();
          info(`compile workspace at ${compileWorkspace}`);
        }
        info(`compiling ${name}${suffix} for ${triple}`);
        await compileFor(triple, `${name}${suffix}`, entryRelative, compileWorkspace);
        const { sha256, size } = await sha256File(outPath);
        artifacts.push({ triple, name, filename, path: outPath, sha256, size });
      }
    }
    return artifacts;
  } finally {
    if (compileWorkspace) {
      await Deno.remove(compileWorkspace, { recursive: true }).catch(() => {});
    }
  }
}

// Where cargo writes its build output. Honors CARGO_TARGET_DIR (the Podman Linux
// driver points it at /tmp/target because the repo is bind-mounted read-only, so
// cargo can't write into REPO_ROOT/target there); falls back to the in-repo
// target/ on the host, where the env is unset.
function cargoTargetDir(): string {
  return Deno.env.get("CARGO_TARGET_DIR") ?? join(REPO_ROOT, "target");
}

// Each helper/speech crate compiles per triple with `cargo --target <triple>`,
// which needs that target's std installed. The drivers pre-install their targets
// (Containerfile / windows-provision.ps1); on the host the second arch (e.g.
// x86_64-apple-darwin on an arm64 mac, now part of the all-targets matrix) may be
// missing. Add each idempotently via rustup before the cargo builds (a no-op when
// already present). Best-effort: if rustup is absent or the add fails, fall
// through and let cargo surface its own clearer error.
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
      // channel-suffixed name so latest's tomat-core-keychain-latest coexists.
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

/** Download + unpack espeak-ng-data once per release run; returns the path to
 *  the extracted `espeak-ng-data/` dir. bzip2 isn't available in Deno, so unpack
 *  via the system `tar` (present on every release host). tar runs with `cwd` set
 *  and relative archive names only: the Windows runner's git-bundled GNU tar
 *  mishandles drive-letter absolute paths (a colon reads as a remote `host:file`,
 *  and backslashes break `-C`), so no absolute path is ever handed to it. */
async function ensureEspeakData(): Promise<string> {
  const tmp = await Deno.makeTempDir({ prefix: "tomat-espeak-" });
  const archiveName = "espeak-ng-data.tar.bz2";
  const archive = join(tmp, archiveName);
  info(`fetching espeak-ng-data`);
  const res = await fetch(ESPEAK_DATA_URL);
  if (!res.ok || !res.body) {
    fail(`espeak-ng-data fetch failed: ${res.status} ${res.statusText}`);
  }
  await Deno.writeFile(archive, res.body);
  // Verify against the pinned sha256 BEFORE unpacking: an upstream asset swap or
  // a release-host MITM must not silently propagate into the tomat-signed speech
  // binary. A legitimate upstream bump updates ESPEAK_DATA_SHA256.
  const { sha256 } = await sha256File(archive);
  if (sha256 !== ESPEAK_DATA_SHA256) {
    fail(
      `espeak-ng-data sha256 mismatch: pinned ${ESPEAK_DATA_SHA256}, got ${sha256}. ` +
        `If upstream legitimately changed the asset, recompute + update ESPEAK_DATA_SHA256.`,
    );
  }
  // Defense for the release host: reject any archive entry that would escape the
  // extraction dir (absolute path or `..` traversal) before extracting.
  const list = await new Deno.Command("tar", {
    args: ["-tjf", archiveName],
    cwd: tmp,
    stdout: "piped",
    stderr: "inherit",
  }).output();
  if (!list.success) fail(`tar -tjf espeak-ng-data failed`);
  for (const entry of new TextDecoder().decode(list.stdout).split("\n")) {
    const name = entry.trim();
    if (!name) continue;
    if (name.startsWith("/") || name.split("/").some((seg) => seg === "..")) {
      fail(`espeak-ng-data archive contains an unsafe path entry: ${name}`);
    }
  }
  const { code } = await new Deno.Command("tar", {
    args: ["-xjf", archiveName],
    cwd: tmp,
    stdout: "inherit",
    stderr: "inherit",
  }).output();
  if (code !== 0) fail(`tar -xjf espeak-ng-data exited ${code}`);
  const dataDir = join(tmp, "espeak-ng-data");
  const st = await Deno.stat(dataDir).catch(() => null);
  if (!st?.isDirectory) {
    fail(`espeak-ng-data/ not found after unpacking ${ESPEAK_DATA_URL}`);
  }
  return dataDir;
}

// The GPU speech variants buildable per triple. Only Linux CUDA is supported:
//  - k2-fsa ships prebuilt CUDA archives for linux-x64 and win-x64, but NO
//    DirectML or CoreML libs (those need an onnxruntime source build, avoided).
//  - The win-x64 CUDA archive omits `onnxruntime.lib`, so the crate's shared
//    link step (`dylib=onnxruntime`) can't link under MSVC; Linux `.so` linking
//    has no separate import lib, so it links against the bundled libonnxruntime.
// So NVIDIA on Linux gets a GPU speech binary; Windows NVIDIA (and every other
// GPU, and Mac) runs speech on CPU (its LLM still gets a GPU llama.cpp build).
// A variant is built ONLY when its prebuilt GPU lib dir is staged in the env var
// below (the same SHERPA_ONNX_LIB_DIR mechanism the win-arm64 CPU build uses);
// otherwise the release ships CPU-only for that triple. The staged archive
// supplies the GPU onnxruntime + provider shared libs, packaged into the tarball
// so they land in bin/lib/tomat-core-speech.
const SPEECH_GPU_VARIANTS: Partial<Record<Triple, BinaryVariant[]>> = {
  "x86_64-unknown-linux-gnu": ["cuda"],
};

/** Env var naming the staged prebuilt GPU sherpa-onnx lib dir for `variant`,
 *  e.g. `SHERPA_ONNX_GPU_LIB_DIR_CUDA`. CI/driver provisioning downloads the
 *  matching k2-fsa GPU archive (`...-linux-x64-gpu` / `...-win-x64-cuda`) and
 *  points this at it (the dir holding the import/`.so` libs the crate links). */
function speechGpuLibDirEnv(variant: BinaryVariant): string {
  return `SHERPA_ONNX_GPU_LIB_DIR_${variant.toUpperCase()}`;
}

/** Recursively yield every file path under `root`. */
async function* walkFiles(root: string): AsyncGenerator<string> {
  for await (const e of Deno.readDir(root)) {
    const p = join(root, e.name);
    if (e.isDirectory) yield* walkFiles(p);
    else if (e.isFile) yield p;
  }
}

/** Build tomat-core-speech for each (host) triple and package it as a `.tar.gz`
 *  of {exe, espeak-ng-data/, shared libs}. The exe inside the archive keeps the
 *  bare kind name the binaries manager's extractor looks for; the archive file
 *  carries the channel suffix (and, for GPU variants, the variant). Always
 *  builds the CPU variant; adds a GPU variant when its prebuilt lib dir is
 *  staged. sha256 is over the archive (verified before extraction). */
async function buildSpeech(triples: Triple[], suffix: string): Promise<SpeechArtifact[]> {
  const out: SpeechArtifact[] = [];
  // Safety net: if a triple is ever marked speech-unavailable (binaries.json),
  // skip it rather than fail the whole core build. None are today (windows-arm64
  // links the sherpa lib via SHERPA_ONNX_LIB_DIR in the guest), so this filters
  // nothing.
  const speechTriples = triples.filter((t) => !binaryUnavailableOnTriple("tomat-core-speech", t));
  for (const skipped of triples.filter((t) => !speechTriples.includes(t))) {
    info(colors.yellow(`skipping speech for ${skipped} (marked unavailable)`));
  }
  if (speechTriples.length === 0) return out;
  const espeakData = await ensureEspeakData();
  for (const triple of speechTriples) {
    // CPU: statically-linked default build (no extra libs).
    out.push(await buildSpeechVariant(triple, "cpu", null, suffix, espeakData));
    // GPU: only when the prebuilt GPU lib dir is staged for this variant.
    for (const variant of SPEECH_GPU_VARIANTS[triple] ?? []) {
      const libDir = Deno.env.get(speechGpuLibDirEnv(variant));
      if (!libDir) {
        info(
          colors.yellow(
            `skipping ${variant} speech for ${triple} (${speechGpuLibDirEnv(variant)} unset)`,
          ),
        );
        continue;
      }
      out.push(await buildSpeechVariant(triple, variant, libDir, suffix, espeakData));
    }
  }
  return out;
}

/** Build + package one speech variant. `gpuLibDir` null = CPU (static default);
 *  set = GPU (build `--no-default-features --features shared` against the staged
 *  prebuilt lib, and bundle that dir's shared libs into the archive). */
async function buildSpeechVariant(
  triple: Triple,
  variant: BinaryVariant,
  gpuLibDir: string | null,
  suffix: string,
  espeakData: string,
): Promise<SpeechArtifact> {
  const isWin = triple.includes("windows");
  const exe = isWin ? ".exe" : "";
  info(`cargo build ${SPEECH_CRATE.name} (${variant}) for ${triple}`);
  const cargoArgs = [
    "build",
    "--release",
    // Signed speech binary builds from the committed, CI-verified lock.
    "--locked",
    "--manifest-path",
    join(REPO_ROOT, SPEECH_CRATE.crateDir, "Cargo.toml"),
    "--target",
    triple,
  ];
  // GPU builds link onnxruntime dynamically (the `shared` feature) against the
  // staged prebuilt GPU lib; CPU keeps the default `static` feature.
  if (gpuLibDir) cargoArgs.push("--no-default-features", "--features", "shared");
  const env: Record<string, string> = {};
  if (gpuLibDir) env.SHERPA_ONNX_LIB_DIR = gpuLibDir;
  const { code } = await new Deno.Command("cargo", {
    args: cargoArgs,
    env,
    stdout: "inherit",
    stderr: "inherit",
  }).output();
  if (code !== 0) {
    fail(`cargo build ${SPEECH_CRATE.name} (${variant}) for ${triple} exited ${code}`);
  }

  // Stage {exe, espeak-ng-data/, shared libs} and pack a gzip tarball. The
  // in-archive exe must be the bare `<kind>(.exe)` the extractor matches.
  const exeIn = `${SPEECH_CRATE.name}${exe}`;
  const staging = await Deno.makeTempDir({ prefix: "tomat-speech-" });
  await Deno.copyFile(join(cargoTargetDir(), triple, "release", exeIn), join(staging, exeIn));
  await copy(espeakData, join(staging, "espeak-ng-data"));
  const tarEntries = [exeIn, "espeak-ng-data"];
  // GPU: copy the prebuilt GPU onnxruntime + provider shared libs into the
  // archive so they land next to the binary at install (bin/lib/<kind>). The
  // k2-fsa archives split runtime libs by OS -- Linux keeps the .so files in
  // `lib/` (the linked dir), Windows keeps the .dll runtime in a sibling `bin/`
  // while `lib/` holds only the import .lib. So collect from the whole archive
  // root (the parent of the linked lib dir), deduping by basename.
  if (gpuLibDir) {
    const seen = new Set<string>();
    for await (const p of walkFiles(dirname(gpuLibDir))) {
      const base = p.split(/[\\/]/).pop() ?? p;
      if (!/\.(so|dylib|dll)(\.\d+)*$/i.test(base) || seen.has(base)) continue;
      seen.add(base);
      await Deno.copyFile(p, join(staging, base));
      tarEntries.push(base);
    }
  }

  const variantTag = variant === "cpu" ? "" : `-${variant}`;
  const filename = `${SPEECH_CRATE.name}${variantTag}${suffix}.tar.gz`;
  const outDir = join(DIST_DIR, triple);
  await ensureDir(outDir);
  const outPath = join(outDir, filename);
  // Pack from within `staging` with relative names so tar never receives a
  // drive-letter path (see ensureEspeakData), then copy the archive to dist.
  const { code: tarCode } = await new Deno.Command("tar", {
    args: ["-czf", filename, ...tarEntries],
    cwd: staging,
    stdout: "inherit",
    stderr: "inherit",
  }).output();
  if (tarCode !== 0) fail(`tar -czf ${filename} exited ${tarCode}`);
  await Deno.copyFile(join(staging, filename), outPath);
  await Deno.remove(staging, { recursive: true }).catch(() => {});

  const { sha256, size } = await sha256File(outPath);
  return { triple, variant, filename, path: outPath, sha256, size };
}

export async function hashWorkers(): Promise<WorkerArtifact[]> {
  const workersDir = join(CORE_DIR, "src/workers");
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
    .filter((a) => a.name === "tomat-core")
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

  // Sign the WHOLE manifest minus the `signature` field. The runtime verifier
  // (packages/tomat-core/src/update/self-updater.ts) reconstructs the same
  // payload by stripping `signature`, so every executed field (binaries,
  // workers, helpers) is authenticated. Do NOT shrink this back to
  // {version,binaries}: that would leave workers[] / helpers[] unsigned.
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
    "user-agent": "tomat-release",
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

/** Build the pinned binaries.json entry for the self-hosted speech binary from
 *  the artifacts this release uploaded. URL points straight at the `.tar.gz`
 *  (already the transport form); sha256 is over that archive. */
function composeSpeechEntry(
  artifacts: SpeechArtifact[],
  version: string,
  storageDomain: string,
  storagePrefix: string,
): BinaryManifestPinnedEntry {
  const platforms = {} as BinaryManifestPinnedEntry["platforms"];
  // Group per triple so a triple with GPU variants emits a per-variant map;
  // a cpu-only triple collapses to the bare pinned shape (matching llama's
  // single-variant triples and what platformVariants reads as `cpu`).
  const byTriple = new Map<Triple, SpeechArtifact[]>();
  for (const a of artifacts) byTriple.set(a.triple, [...(byTriple.get(a.triple) ?? []), a]);
  for (const [triple, arts] of byTriple) {
    const target = (a: SpeechArtifact): PinnedTarget => ({
      url: `https://${storageDomain}/${storagePrefix}${version}/${a.triple}/${a.filename}`,
      sha256: a.sha256,
    });
    if (arts.length === 1 && arts[0].variant === "cpu") {
      platforms[triple] = target(arts[0]);
      continue;
    }
    const variants: Partial<Record<BinaryVariant, PinnedTarget>> = {};
    for (const a of arts) variants[a.variant] = target(a);
    platforms[triple] = variants;
  }
  return { version, platforms };
}

async function composeBinaryManifest(
  privateKey: Uint8Array,
  channel: ReleaseChannel,
  version: string,
  speech: BinaryManifestPinnedEntry,
): Promise<BinaryManifest> {
  const binaries = {} as Record<BinaryKind, BinaryManifestEntry>;
  for (const kind of BINARY_KINDS) {
    const resolver = UPSTREAM_BINARIES[kind];
    // Self-hosted kinds (tomat-core-speech) have no upstream resolver; pinned
    // from this release's own artifacts after the loop.
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

  // Pin self-hosted binaries (built host-only + uploaded by apply()) on every
  // channel; ours, so there's no upstream release to resolve.
  binaries["tomat-core-speech"] = speech;

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

// Packages composed into the core release item: the Deno service, shared types,
// the eager helper crates, and the on-demand speech crate. CORE_HASH_INPUTS is
// derived from this list (src + manifest per package), so adding a helper crate
// here cannot silently skip the source-hash gate. Lockfiles are intentionally
// excluded (a client-only dep bump touches the root lock but not core); declared
// deps live in each deno.json / Cargo.toml, which ARE hashed.
const CORE_PACKAGES = [
  "core",
  "core-engine",
  "shared",
  "core-keychain",
  "core-updater",
  "core-hwinfo",
  "core-ptyhost",
  "core-speech",
];
const CORE_HASH_INPUTS = packagesHashInputs(CORE_PACKAGES);

// The dist/<triple>/ binaries buildAll + buildHelpers emit for this channel:
// tomat-core plus the four helpers, each with the channel suffix. The unified
// build hash-checks these so a wiped or swapped artifact forces a rebuild.
// Defaults to the host triple; a cross-platform build passes the triples it
// built so the cursor tracks every arch, not just the host's.
export function coreOutputs(
  channel: ReleaseChannel,
  triples: Triple[] = [detectHostTriple()],
): string[] {
  const suffix = channelBinSuffix(channel);
  const names = ["tomat-core", ...HELPER_CRATES.map((h) => h.name)];
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
  bumpHint: "packages/tomat-core/src/config.ts (CORE_VERSION)",

  version: readCoreVersion,
  versionFile: CONFIG_PATH,
  bumpVersion: bumpCoreVersion,

  sourceHash(_channel: ReleaseChannel): Promise<string> {
    // config.ts carries CORE_VERSION; drop it from the tree hash and fold it back
    // in with the version blanked, so a lone version bump does not re-trigger a
    // core release while a real config.ts change still does.
    const configRel = rel(CONFIG_PATH);
    return hashPaths([
      ...CORE_HASH_INPUTS.map((p) => ({
        path: join(REPO_ROOT, p),
        exclude: (r: string) => r.endsWith(".test.ts") || r === configRel,
      })),
      { path: CONFIG_PATH, transform: stripCoreVersion },
    ]);
  },

  buildOutputs(channel: ReleaseChannel): Promise<string[]> {
    return Promise.resolve(coreOutputs(channel));
  },

  async apply(env: DeployEnv, channel: ReleaseChannel, opts: ApplyOpts): Promise<void> {
    const suffix = channelBinSuffix(channel);
    const version = await readCoreVersion();
    // Prebuilt mode (CI publish): merge the bundles the build runners produced
    // (their dist/<triple>/ files already collected under DIST_DIR) and hash the
    // platform-independent workers from this host's checkout. No build.
    // All-targets mode: build host triples here + the rest in their environments.
    // Host-only mode: today's single-host build.
    const built = opts.prebuilt
      ? await mergeCoreBundles(opts.prebuilt.coreBundles, await hashWorkers())
      : opts.environments?.length
        ? await buildCoreUnified(opts.triples, suffix, channel, opts.environments)
        : await buildCoreArtifacts(opts.triples, suffix);
    await composeAndUploadCore(env, channel, version, built, opts);
    // Native installers are NOT built here: they belong to the client item
    // (client.ts), which wraps the just-published core artifacts and its own
    // bundle into the host OS's installers. In CI each build runner packages
    // its own OS's installers (ci-build.ts).
  },
};

// Build the core artifacts for `triples` across this host + the given build
// environments: the host compiles its own OS's triples directly; every other
// triple is routed to an environment (started on demand, torn down after) that
// builds it and ships the artifacts back into the host's dist/. The results are
// merged into one CoreBuildArtifacts for composeAndUploadCore. Triples with no
// available environment are reported and dropped (the manifests tolerate a
// partial platform set).
export async function buildCoreUnified(
  triples: Triple[],
  suffix: string,
  channel: ReleaseChannel,
  environments: BuildEnvironment[],
): Promise<CoreBuildArtifacts> {
  const routing = await routeTriples(triples, environments);
  reportRouting(routing);

  const hostBuilt = await buildCoreArtifacts(routing.host, suffix);

  const bundles: ArtifactBundle[] = [];
  for (const { env, triples: envTriples } of routing.byEnv) {
    const bundle = await withEnvironment(env, () =>
      env.buildCore({ triples: envTriples, channel, suffix }),
    );
    bundles.push(bundle);
  }
  // Re-anchor + verify the environments' artifacts (already copied into dist/).
  // Workers are platform-independent, so the host's are authoritative.
  const fromEnvs = await mergeCoreBundles(bundles, []);
  return {
    artifacts: [...hostBuilt.artifacts, ...fromEnvs.artifacts],
    helpers: [...hostBuilt.helpers, ...fromEnvs.helpers],
    speech: [...hostBuilt.speech, ...fromEnvs.speech],
    workers: hostBuilt.workers,
  };
}

// Build everything the core release item needs for a set of triples, in the
// current environment: the deno-compiled binaries, the four native helpers, the
// speech sidecar, and the (platform-independent) worker hashes. This is the half
// that runs INSIDE each build environment (host, a Podman container, the Windows
// VM); composeAndUploadCore aggregates the results and runs once on the host.
export async function buildCoreArtifacts(
  triples: Triple[],
  suffix: string,
): Promise<CoreBuildArtifacts> {
  step(`Building Deno binaries (${triples.length} triples)`);
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

  step(`Building speech binary (${triples.length} triples)`);
  const speech = await buildSpeech(triples, suffix);
  for (const s of speech) {
    ok(`${s.triple}/${s.filename}  ${humanBytes(s.size)}  ${s.sha256.slice(0, 12)}…`);
  }

  step("Hashing worker scripts");
  const workers = await hashWorkers();
  for (const w of workers) {
    ok(`workers/${w.name}  ${humanBytes(w.size)}  ${w.sha256.slice(0, 12)}…`);
  }

  return { artifacts, helpers, speech, workers };
}

// Compose + sign core.json/binaries.json over the given (possibly cross-env)
// artifact set, then upload the artifacts + manifests to R2. Runs ONCE on the
// host: composeCoreManifest/composeBinaryManifest carry no R2 carry-forward, so
// the arrays passed here must already be the full union across every triple.
export async function composeAndUploadCore(
  env: DeployEnv,
  channel: ReleaseChannel,
  version: string,
  built: CoreBuildArtifacts,
  opts: ApplyOpts,
): Promise<void> {
  const { artifacts, helpers, speech, workers } = built;
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
  const binaryManifest = await composeBinaryManifest(
    env.signingPrivateKey,
    channel,
    version,
    composeSpeechEntry(speech, version, env.storageDomain, prefix),
  );
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

  step(`Uploading speech binary to R2 bucket "${env.r2Bucket}"`);
  await mapPool(speech, R2_CONCURRENCY, async (s) => {
    // Already a .tar.gz (its own transport form); upload as-is, no extra gzip.
    const key = `${prefix}${version}/${s.triple}/${s.filename}`;
    info(`uploading ${key}  (${humanBytes(s.size)})`);
    await r2Put(env, key, s.path, "application/gzip");
    opts.recordVersionedKey?.(key);
    opts.recordReleaseAsset?.(s.path, `${s.triple}_${s.filename}`);
  });
  ok(`uploaded ${speech.length} speech binaries`);

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
