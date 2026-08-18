// Release item: loca-core (the Rust crate `loca-core-rs`) + the surviving native
// helper crate (updater) + the worker .ts files, composed into signed core.json
// and binaries.json and uploaded to R2. Versioned via CORE_VERSION in
// packages/loca-core/src/config.ts.
//
// The core was a `deno compile` binary until the Rust port cut over. What that
// removed, and why it is gone rather than kept beside the new path: there is no
// dual-shipping period (D-000), so a second build path would be a second thing
// to keep green with nothing shipping from it. `setupCompileWorkspace()` went
// with it; it existed only to keep `deno compile` from embedding the whole
// workspace's npm graph, a problem cargo does not have.
//
// Deno itself does NOT go away. Extension tools still run as sandboxed `deno`
// subprocesses, so the pinned runtime is still a consent-gated download and
// `tool-worker.ts` still ships beside the binary.

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
} from "../../packages/loca-shared/src/domain/model.ts";
import {
  assetVariants,
  BINARY_KINDS,
  binaryUnavailableOnTriple,
  UPSTREAM_BINARIES,
} from "../../packages/loca-shared/src/domain/model.ts";
import {
  type ApplyOpts,
  bumpCoreVersion,
  channelBinSuffix,
  channelManifestDir,
  channelStoragePrefix,
  colors,
  CONFIG_PATH,
  CORE_CONFIG_RS,
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

// One helper left. The Rust core absorbed the keychain (`secrets/**`), the
// hardware probe (`models/hardware.rs`, via `sysinfo`) and the PTY host
// (`extensions/pty/**`), so those three crates are deleted rather than unused.
//
// The updater stays a SEPARATE BINARY on purpose and is not an oversight: it is
// what swaps the core executable during self-update, and a process cannot
// replace the file it is currently running from on Windows. The boundary is the
// feature.
const HELPER_CRATES: Array<{
  name: "loca-core-updater";
  crateDir: string;
}> = [{ name: "loca-core-updater", crateDir: "packages/loca-core-updater" }];

// loca-core-speech is OURS but, unlike the eager helpers, it's consent-gated +
// on-demand: built host-only here, packaged with its espeak-ng-data, and pinned
// into binaries.json (not core.json) so it downloads only when STT/TTS is on.
const SPEECH_CRATE = {
  name: "loca-core-speech",
  crateDir: "packages/loca-core-speech",
} as const;

// Canonical standalone espeak-ng-data (the Kokoro phonemizer data the sherpa
// static lib needs at runtime but doesn't bundle). bzip2, so unpacked via the
// system `tar`, then repacked into the speech binary's gzip archive.
const ESPEAK_DATA_URL =
  "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/espeak-ng-data.tar.bz2";

// Pinned sha256 of the espeak-ng-data archive. The URL above is a MUTABLE rolling
// tag, and these bytes get baked into the loca-signed speech binary, so the
// archive is verified against this hash before unpacking. If upstream legitimately
// republishes the asset, recompute and update this constant in the same commit.
const ESPEAK_DATA_SHA256 = "4135ccf82e1f40613491c0874d4945ae9e9c7840933d8e25a6f9e003d9ebf533";

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
// workspace. `loca-core` imports `@loca/shared`, `@loca/core-engine`, and
// `@loca/model-catalog` via relative paths, so all must be present for
/// The Rust core crate, and the one binary target it produces.
const CORE_CRATE_DIR = "packages/loca-core-rs";
const CORE_BIN = "loca-core";

/// Features a SHIPPING core is built with.
///
/// Both are off by default in the crate, and that default is for CI rather than
/// for releases: each compiles a vendored third-party C/C++ tree, and a fresh
/// checkout has no `vendor/`, so a default-on build script would turn every
/// clippy job red. A release is the case that must have them.
///
/// Shipping without `local-inference` would be the loudest possible regression
/// (no on-device model at all) and shipping without `local-speech` would
/// silently drop local STT/TTS back to external providers, so neither is
/// optional here and neither is a flag.
const CORE_FEATURES = ["local-inference", "local-speech"] as const;

/// llama.cpp is vendored by script rather than by submodule, and `vendor/` is
/// gitignored, so a release host's fresh checkout does not have it.
///
/// Idempotent and cheap when the tree is already at the pin. Run rather than
/// assumed, because `build.rs` reads `vendor/.llama-pin` and a missing tree
/// surfaces as a compiler error rather than as a missing prerequisite.
///
/// **espeak-ng-data is deliberately NOT fetched here.** Its archive is
/// COMMITTED and pulled in with `include_bytes!`, so the build needs no network
/// for it; `scripts/vendor-espeak.ts` regenerates and verifies that committed
/// file and is a maintenance tool, not a build step. Calling it from here would
/// add an upstream fetch to every release build for a file already in the tree.
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
/// Not packaging polish. `GGML_BACKEND_DL` (D-103) makes every backend a
/// separately loaded shared library, **including the CPU one**: with none of
/// these present the process registers zero compute devices and cannot run a
/// model at all (D-106). `backends::directories` looks for them in the install
/// directory, which is wherever the binary is unpacked, and `build.rs` stages
/// them next to cargo's artifacts so the relative rpath (D-116) resolves.
///
/// Returns the filenames copied, so the caller can fail on an empty result
/// rather than shipping a core that cannot compute.
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
          `so this core would register zero compute devices and run no model (D-103, D-106).`,
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

// Each helper/speech crate compiles per triple with `cargo --target <triple>`,
// which needs that target's std installed. Every build is native, so the host
// triple is normally already present. Add each idempotently via rustup before the
// cargo builds (a no-op when already present). Best-effort: if rustup is absent
// or the add fails, fall through and let cargo surface its own clearer error.
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
      // channel-suffixed name so latest's loca-core-keychain-latest coexists.
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
  const tmp = await Deno.makeTempDir({ prefix: "loca-espeak-" });
  const archiveName = "espeak-ng-data.tar.bz2";
  const archive = join(tmp, archiveName);
  info(`fetching espeak-ng-data`);
  const res = await fetch(ESPEAK_DATA_URL);
  if (!res.ok || !res.body) {
    fail(`espeak-ng-data fetch failed: ${res.status} ${res.statusText}`);
  }
  await Deno.writeFile(archive, res.body);
  // Verify against the pinned sha256 BEFORE unpacking: an upstream asset swap or
  // a release-host MITM must not silently propagate into the loca-signed speech
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
// so they land in bin/lib/loca-core-speech.
const SPEECH_GPU_VARIANTS: Partial<Record<Triple, BinaryVariant[]>> = {
  "x86_64-unknown-linux-gnu": ["cuda"],
};

/** Env var naming the staged prebuilt GPU sherpa-onnx lib dir for `variant`,
 *  e.g. `SHERPA_ONNX_GPU_LIB_DIR_CUDA`. The CI runner's provisioning step downloads the
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

/** Build loca-core-speech for each (host) triple and package it as a `.tar.gz`
 *  of {exe, espeak-ng-data/, shared libs}. The exe inside the archive keeps the
 *  bare kind name the binaries manager's extractor looks for; the archive file
 *  carries the channel suffix (and, for GPU variants, the variant). Always
 *  builds the CPU variant; adds a GPU variant when its prebuilt lib dir is
 *  staged. sha256 is over the archive (verified before extraction). */
async function buildSpeech(triples: Triple[], suffix: string): Promise<SpeechArtifact[]> {
  const out: SpeechArtifact[] = [];
  // Safety net: if a triple is ever marked speech-unavailable (binaries.json),
  // skip it rather than fail the whole core build. None are today (windows-arm64
  // links the sherpa lib via SHERPA_ONNX_LIB_DIR, staged by the CI runner), so
  // this filters nothing.
  const speechTriples = triples.filter((t) => !binaryUnavailableOnTriple("loca-core-speech", t));
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
  const staging = await Deno.makeTempDir({ prefix: "loca-speech-" });
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

  // Sign the WHOLE manifest minus the `signature` field. The runtime verifier
  // (packages/loca-core/src/update/self-updater.ts) reconstructs the same
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
    // Self-hosted kinds (loca-core-speech) have no upstream resolver; pinned
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
  binaries["loca-core-speech"] = speech;

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
// **What actually composes the shipped artifact**, which since the build
// cutover is the Rust crate rather than the TypeScript one. Getting this list
// wrong is silent in the worst direction: the source-hash gate decides whether a
// release is NEEDED, so a list that watched the old core would let a change to
// the code that actually ships go out under an unchanged hash, or not go out at
// all.
//
// `core` and `core-engine` are deliberately ABSENT even though both still exist:
// they are retained as the conformance oracle and ship nothing, so hashing them
// would trigger core releases for edits that cannot reach a user.
//
// `core` is the one exception and it is listed for a single file: it still holds
// `src/workers/tool-worker.ts`, which IS shipped beside the binary and which the
// Rust core spawns under the pinned `deno`. That file is the last thread tying a
// release to the retained package; if it moves, `core` leaves this list.
const CORE_PACKAGES = ["core-rs", "shared", "core-updater", "core"];
// Two files of the Rust crate live OUTSIDE `src/` and both change the artifact,
// so `pkgHashInputs`'s src+manifest pair is not sufficient for it:
// `build.rs` decides how llama.cpp is linked and which backends are staged, and
// `schema.sql` is the database the core opens. A change to either with an
// unchanged hash would publish a core that behaves differently under the same
// version.
const CORE_HASH_INPUTS = [
  ...packagesHashInputs(CORE_PACKAGES),
  "packages/loca-core-rs/build.rs",
  "packages/loca-core-rs/schema.sql",
];

// The dist/<triple>/ binaries buildAll + buildHelpers emit for this channel:
// loca-core plus the four helpers, each with the channel suffix. `deno task
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
  bumpHint:
    "packages/loca-core-rs/src/config.rs (CORE_VERSION), mirrored in packages/loca-core/src/config.ts",

  version: readCoreVersion,
  // The RUST constant: it is the one the shipped binary reports, so it is the
  // one an update check compares against. `bumpCoreVersion` keeps the oracle's
  // copy in step and `readCoreVersion` refuses a tree where they disagree.
  versionFile: CORE_CONFIG_RS,
  bumpVersion: bumpCoreVersion,

  sourceHash(_channel: ReleaseChannel): Promise<string> {
    // Both config files carry CORE_VERSION; drop each from the tree hash and
    // fold it back in with the version blanked, so a lone version bump does not
    // re-trigger a core release while a real change to either still does.
    //
    // Two entries rather than one since the cutover: missing the Rust half would
    // make every version bump look like a source change to the crate that
    // actually ships, and every release would then chase its own tail.
    const configRel = rel(CONFIG_PATH);
    const rustConfigRel = rel(CORE_CONFIG_RS);
    return hashPaths([
      ...CORE_HASH_INPUTS.map((p) => ({
        path: join(REPO_ROOT, p),
        exclude: (r: string) => r.endsWith(".test.ts") || r === configRel || r === rustConfigRel,
      })),
      { path: CONFIG_PATH, transform: stripCoreVersion },
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

// Build everything the core release item needs for a set of triples, natively:
// the deno-compiled binaries, the native helpers, the speech sidecar, and the
// (platform-independent) worker hashes. This is the half that runs on each build
// runner (and in `deno task build`); composeAndUploadCore aggregates the results
// and runs once on the publish coordinator.
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
// publish coordinator: composeCoreManifest/composeBinaryManifest carry no R2
// carry-forward, so the arrays passed here must already be the full union across
// every triple the build matrix staged.
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
