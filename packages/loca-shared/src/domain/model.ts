// Model + binary types shared across core and client.

// All triples the binary manifest publishes. Core only ever downloads the
// host triple's binary at install time.
export const TRIPLES = [
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "aarch64-pc-windows-msvc",
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
] as const;

export type Triple = (typeof TRIPLES)[number];

export const BINARY_KINDS = ["llama-server", "loca-core-speech", "deno"] as const;

export type BinaryKind = (typeof BINARY_KINDS)[number];

// GPU-backend build variants a binary may ship. `cpu` is the guaranteed
// fallback present for every triple; the rest are hardware-specific builds
// (llama.cpp: metal/vulkan/cuda/rocm/hip; sherpa-onnx: cuda/directml/coreml).
// A device selects the best variant its hardware supports and the manifest
// offers, always terminating at `cpu` (see domain/variant.ts).
export const BINARY_VARIANTS = [
  "cpu",
  "metal",
  "vulkan",
  "cuda",
  "rocm",
  "hip",
  "directml",
  "coreml",
] as const;

export type BinaryVariant = (typeof BINARY_VARIANTS)[number];

// HF-style spec: "@user/repo/branch/file". Branch may be "main".
// The model storage path is derived by stripping the leading "@" and treating
// the remainder as a relative path under ~/.loca/core/models/.
export type ModelSpec = `@${string}/${string}/${string}/${string}`;

export interface ModelEntry {
  source: ModelSpec | string;
  relPath: string;
  absPath: string;
  sizeBytes: number;
}

export type DownloadStatus = "Pending" | "Downloading" | "Completed" | "Error" | "Cancelled";

// Wire shape served by /api/v1/models/downloads and broadcast over WS as
// `downloads.snapshot`.
export interface DownloadEntry {
  id: string;
  source: string;
  destination: "models" | "binaries" | "extensions";
  relPath: string;
  absPath: string;
  filename: string;
  groupId: string;
  sizeBytes?: number;
  downloadedBytes: number;
  status: DownloadStatus;
  error?: string;
  addedAtMs: number;
}

export interface DownloadPlan {
  source: string;
  alreadyHave: boolean;
  sizeHint?: number;
  /** Resolved version for sidecar-binary plans (e.g. an upstream release tag).
   *  Unset for model files. */
  version?: string;
  /** The last attempt to prepare this download failed; the confirm modal shows a
   *  retry note instead of a size. Carried through from the requirements
   *  snapshot's `RequiredFile.error`. */
  error?: string;
}

// A binary manifest entry is one of two shapes:
//
//  - Pinned (stable channel): exact per-triple URLs + sha256, resolved at
//    release time. The signed manifest commits to the exact bytes.
//  - Resolver (latest channel): an upstream GitHub repo + per-triple asset name
//    patterns. The core resolves the LATEST release at runtime, so upstream
//    updates reach latest users without us re-releasing. The signed manifest
//    commits to the repo + patterns (not the version); the download is
//    verified against GitHub's published sha256 digest.
// One downloadable artifact: a primary asset-name pattern plus optional
// companion archives (e.g. the Windows CUDA `cudart` runtime) extracted into the
// same lib dir. "{tag}" expands to the release tag_name in every pattern.
export interface VariantAsset {
  asset: string;
  extra?: string[];
}

// A triple maps to either a single bare asset pattern (single-variant, treated
// as `cpu`) or a per-variant map. A variant map MUST include `cpu` (the
// guaranteed fallback). Read through `assetVariants` so both shapes normalize.
export type TripleAsset = string | Partial<Record<BinaryVariant, string | VariantAsset>>;

// One resolved, pinned download: the primary URL + sha256 plus any companion
// archives (extracted libs-only into bin/lib/<kind>).
export interface PinnedTarget {
  url: string;
  sha256: string;
  extras?: { url: string; sha256: string }[];
}

// A triple maps to either a single bare pinned target (single-variant, treated
// as `cpu`) or a per-variant map. Read through `platformVariants`.
export type VariantPlatform = PinnedTarget | Partial<Record<BinaryVariant, PinnedTarget>>;

export interface BinaryManifestPinnedEntry {
  version: string;
  platforms: Record<Triple, VariantPlatform>;
}

// `assets` maps a triple to the upstream asset name(s); "{tag}" expands to the
// release's tag_name (e.g. "llama-{tag}-bin-macos-arm64.tar.gz").
export interface UpstreamResolver {
  repo: string;
  assets: Partial<Record<Triple, TripleAsset>>;
  /** When set, resolve this exact release tag instead of the latest. Pins a
   *  binary on EVERY channel (latest/dev resolve at runtime, stable pins from
   *  the same tag at release time), so bumping it is a deliberate edit. */
  pinnedTag?: string;
}

/** Normalize a triple's asset config into a per-variant map. A bare string is a
 *  single `cpu` variant; a bare per-variant string value becomes `{ asset }`.
 *  Everything downstream (selection, resolution, manifest generation) reads
 *  through this, so single-variant kinds (deno, mac llama) need no special case. */
export function assetVariants(
  a: TripleAsset | undefined,
): Partial<Record<BinaryVariant, VariantAsset>> {
  if (a === undefined) return {};
  if (typeof a === "string") return { cpu: { asset: a } };
  const out: Partial<Record<BinaryVariant, VariantAsset>> = {};
  for (const [v, val] of Object.entries(a)) {
    if (val === undefined) continue;
    out[v as BinaryVariant] = typeof val === "string" ? { asset: val } : val;
  }
  return out;
}

/** Normalize a pinned platform entry into a per-variant map. A bare
 *  `PinnedTarget` (has `url`) is the `cpu` variant. Mirror of {@link assetVariants}
 *  for the pinned (stable-channel) manifest shape. */
export function platformVariants(
  p: VariantPlatform | undefined,
): Partial<Record<BinaryVariant, PinnedTarget>> {
  if (!p) return {};
  return "url" in p ? { cpu: p } : p;
}

export interface BinaryManifestResolverEntry {
  resolver: UpstreamResolver;
}

// Single source of truth for the upstream resolver config per sidecar binary:
// the GitHub repo + per-triple asset-name pattern ("{tag}" expands to the
// release's tag_name). Consumed by:
//  - the release script: STABLE pins the current latest (URL + sha256) at
//    release time; LATEST embeds the resolver verbatim so the core resolves the
//    latest release at runtime.
//  - a DEV core: builds resolver entries from this in-code (dev has no
//    published manifest), so a from-source build pulls the latest upstream
//    sidecar release exactly like latest.
// Partial: self-hosted kinds (loca-core-speech) have no upstream resolver. The
// release builds and pins them directly into binaries.json (scripts/release/
// core.ts composeBinaryManifest), so they are absent from this map.
export const UPSTREAM_BINARIES: Partial<Record<BinaryKind, UpstreamResolver>> = {
  "llama-server": {
    repo: "ggml-org/llama.cpp",
    // Per-variant asset maps: `cpu` is always present (the guaranteed fallback);
    // GPU variants are the upstream GPU builds. macOS bakes Metal into the single
    // default build, so mac stays a bare (cpu-keyed) string. See domain/variant.ts
    // for how a device picks the best offered variant for its detected backend.
    assets: {
      "aarch64-apple-darwin": "llama-{tag}-bin-macos-arm64.tar.gz",
      "x86_64-apple-darwin": "llama-{tag}-bin-macos-x64.tar.gz",
      "x86_64-unknown-linux-gnu": {
        cpu: "llama-{tag}-bin-ubuntu-x64.tar.gz",
        vulkan: "llama-{tag}-bin-ubuntu-vulkan-x64.tar.gz",
        rocm: "llama-{tag}-bin-ubuntu-rocm-7.2-x64.tar.gz",
      },
      "aarch64-unknown-linux-gnu": {
        cpu: "llama-{tag}-bin-ubuntu-arm64.tar.gz",
        vulkan: "llama-{tag}-bin-ubuntu-vulkan-arm64.tar.gz",
      },
      "x86_64-pc-windows-msvc": {
        cpu: "llama-{tag}-bin-win-cpu-x64.zip",
        vulkan: "llama-{tag}-bin-win-vulkan-x64.zip",
        // CUDA ships a separate cudart runtime archive extracted alongside.
        cuda: {
          asset: "llama-{tag}-bin-win-cuda-13.3-x64.zip",
          extra: ["cudart-llama-bin-win-cuda-13.3-x64.zip"],
        },
        hip: "llama-{tag}-bin-win-hip-radeon-x64.zip",
      },
      "aarch64-pc-windows-msvc": "llama-{tag}-bin-win-cpu-arm64.zip",
    },
  },
  deno: {
    repo: "denoland/deno",
    // Pinned on every channel: tool-worker permission prompts are parsed
    // from this version's prompt wording (loca-core/src/extensions/
    // prompt-parser.ts). Bump deliberately and re-run the live-probe test
    // (prompt-live-probe.test.ts) against the new release first.
    pinnedTag: "v2.8.2",
    assets: {
      "aarch64-apple-darwin": "deno-aarch64-apple-darwin.zip",
      "x86_64-apple-darwin": "deno-x86_64-apple-darwin.zip",
      "aarch64-unknown-linux-gnu": "deno-aarch64-unknown-linux-gnu.zip",
      "x86_64-unknown-linux-gnu": "deno-x86_64-unknown-linux-gnu.zip",
      "x86_64-pc-windows-msvc": "deno-x86_64-pc-windows-msvc.zip",
      "aarch64-pc-windows-msvc": "deno-aarch64-pc-windows-msvc.zip",
    },
  },
};

export type BinaryManifestEntry = BinaryManifestPinnedEntry | BinaryManifestResolverEntry;

export function isResolverEntry(e: BinaryManifestEntry): e is BinaryManifestResolverEntry {
  return (e as BinaryManifestResolverEntry).resolver !== undefined;
}

export interface BinaryManifest {
  schemaVersion: 1;
  // The core release version this manifest shipped with. Monotonic anchor the
  // runtime uses to refuse a strictly-older (replayed) signed manifest.
  version: string;
  binaries: Record<BinaryKind, BinaryManifestEntry>;
  signature: string; // base64 Ed25519 over canonical JSON of the manifest minus signature
}

export interface BinaryStatus {
  kind: BinaryKind;
  version: string;
  installed: boolean;
  path?: string;
  /** The GPU variant currently installed on disk (`cpu` for single-variant
   *  kinds; undefined when not installed). */
  variant?: BinaryVariant;
  /** The desired variant captured at the last install (what that install AIMED
   *  for). Equals `desiredVariant` in the normal case; differs from `variant`
   *  only when the ideal wasn't resolvable upstream and the install degraded.
   *  Present-ness is judged by `target === desiredVariant` (not
   *  `variant === desiredVariant`), so an unavoidable fallback settles instead of
   *  reading as perpetually-missing. Undefined when not installed. */
  target?: BinaryVariant;
  /** The variant this device should have, given detected hardware + the backend
   *  override (the ideal). A concrete better variant becoming installable surfaces
   *  through the update path, not the missing-requirement path. */
  desiredVariant?: BinaryVariant;
}

// Resolved download metadata for a not-yet-installed binary, surfaced in the
// startup download confirmation. `version` is the resolved release (or
// "unknown" when it couldn't be resolved); `sizeBytes` is the download size
// when known (GitHub asset size on latest, HEAD Content-Length on stable).
export interface BinaryProbeResult {
  kind: BinaryKind;
  version: string;
  sizeBytes?: number;
}

// --- requirements -----------------------------------------------------------
//
// The single source of truth for "what files does the current configuration
// need" (models + sidecar binaries). The core computes a RequirementsSnapshot
// from these and broadcasts it; the client renders one pending-downloads popup.

/** Synthetic source prefix for a sidecar binary surfaced in the same list as
 *  HF model files (e.g. `binary:llama-server`). */
export const BINARY_SOURCE_PREFIX = "binary:";

export function binarySource(kind: BinaryKind): string {
  return `${BINARY_SOURCE_PREFIX}${kind}`;
}
export function isBinarySource(source: string): boolean {
  return source.startsWith(BINARY_SOURCE_PREFIX);
}
export function binarySourceToKind(source: string): BinaryKind {
  return source.slice(BINARY_SOURCE_PREFIX.length) as BinaryKind;
}

export type RequirementGroup = "llm" | "stt" | "tts" | "embed";

/** One thing the current config needs present on disk. Shaped so the `missing`
 *  subset is directly renderable by the download confirm modal. */
export interface RequiredFile {
  /** HF model spec ("@u/r/branch/file"), or `binary:<kind>`. */
  source: string;
  type: "model" | "binary";
  group: RequirementGroup | "binary";
  present: boolean;
  /** A binary with no upstream asset on this platform. Kept in `required` for
   *  visibility but excluded from `missing` so it never blocks the app. */
  unavailable?: boolean;
  /** The last attempt to prepare/download/install this file failed and it needs
   *  a retry (e.g. an upstream binary that could not be resolved). Still counted
   *  in `missing` (it blocks), but the UI shows the reason + a retry affordance
   *  instead of a normal, sizeless "Download" line. Distinct from `unavailable`
   *  (platform-impossible, non-blocking). */
  error?: string;
  sizeHint?: number;
  version?: string;
}

export interface RequirementsSnapshot {
  required: RequiredFile[];
  /** Subset of `required` that is not present and not unavailable. Drives the
   *  popup and the app's pending gate. */
  missing: RequiredFile[];
}

export interface RequiredModelRef {
  source: string;
  group: RequirementGroup;
}

// The schema defaults for the two enable flags, applied here so a SPARSE
// settings object (the on-disk file stores only non-default values) yields the
// same answer as a resolved one. Both default to false (see
// settings/groups/{stt,tts}.ts), so a fresh install needs no speech model or
// binary to send a text message; voice is opt-in. A provider absent or set to
// anything other than "external" means local.
const STT_ENABLED_DEFAULT = false;
const TTS_ENABLED_DEFAULT = false;

/** True when speech-to-text runs on the local `loca-core-speech` binary (it is
 *  enabled and not pointed at an external service). The single predicate behind
 *  every gate that branches local-vs-external STT (model + binary requirements,
 *  the speech sidecar's desired state), so they cannot drift on an absent flag,
 *  an absent provider, or a malformed value, whether fed sparse or resolved
 *  settings. */
export function sttUsesLocal(s: Record<string, unknown>): boolean {
  const enabled = s["stt.enabled"] ?? STT_ENABLED_DEFAULT;
  return !!enabled && s["stt.provider"] !== "external";
}

/** True when text-to-speech runs on the local `loca-core-speech` binary.
 *  Mirror of {@link sttUsesLocal} for the TTS gates. */
export function ttsUsesLocal(s: Record<string, unknown>): boolean {
  const enabled = s["tts.enabled"] ?? TTS_ENABLED_DEFAULT;
  return !!enabled && s["tts.provider"] !== "external";
}

/** Sidecar binary kinds the settings require: `deno` always (runs the tool
 *  worker); `llama-server` always (local chat when the LLM provider is local,
 *  plus embeddings for tool-relevance, which always run on a second llama-server
 *  instance); `loca-core-speech` iff local STT or TTS is enabled (one binary
 *  serves both Whisper STT and Kokoro TTS). */
export function requiredBinaryKinds(s: Record<string, unknown>): BinaryKind[] {
  const out: BinaryKind[] = ["deno", "llama-server"];
  if (sttUsesLocal(s) || ttsUsesLocal(s)) out.push("loca-core-speech");
  return out;
}

/** Triples a self-hosted (no upstream resolver) binary cannot run on.
 *  loca-core-speech statically links sherpa-onnx; upstream ships a static lib
 *  for every shipped triple. windows-arm64 is the one the sherpa-onnx-sys crate's
 *  download map omits, so the build supplies it via SHERPA_ONNX_LIB_DIR (staged by
 *  the "Stage sherpa-onnx win-arm64 lib" step in .github/workflows/release.yml).
 *  No triple is unsupported today; the map stays for any future per-triple gap. */
const SELF_HOSTED_UNSUPPORTED: Partial<Record<BinaryKind, ReadonlySet<Triple>>> = {};

/** True when `kind` can never be installed on `triple` (so it's marked
 *  `unavailable` rather than perpetually `missing`). Resolver-backed kinds have
 *  no upstream asset for the triple; self-hosted kinds (pinned directly into
 *  binaries.json) are available everywhere except their statically-known
 *  unsupported triples. */
export function binaryUnavailableOnTriple(kind: BinaryKind, triple: Triple): boolean {
  const resolver = UPSTREAM_BINARIES[kind];
  if (resolver) return Object.keys(assetVariants(resolver.assets[triple])).length === 0;
  return SELF_HOSTED_UNSUPPORTED[kind]?.has(triple) ?? false;
}

// Core update manifest (separate from binaries).
//
// `binaries` carries the per-triple loca-core executables. `workers` carries
// the platform-independent .ts file(s) (just toolWorker now) spawned at runtime
// by the core; shipping them separately from the compiled binary keeps
// deno-compile's embedded-dependency set scoped to the core's own graph rather
// than the whole workspace. `helpers` carries per-triple binaries that ship next
// to core in ~/.loca/<channel>/core/bin/ and are installed (and swapped on
// self-update) by the same code path: loca-core-keychain (Rust crate) and
// loca-core-updater (Rust crate; the binary that performs the swap). They
// are invoked via subprocess.
export interface CoreManifest {
  schemaVersion: 1;
  version: string;
  binaries: Array<{ triple: Triple; url: string; sha256: string }>;
  workers: Array<{ name: string; url: string; sha256: string }>;
  helpers: Array<{ name: string; triple: Triple; url: string; sha256: string }>;
  signature: string;
}

// Signed manifest for the CDN-distributed built-in extension. Mirrors CoreManifest:
// the whole object minus `signature` is canonicalized + Ed25519-signed at release
// time, and the runtime verifier reconstructs the same payload by stripping
// `signature`. `version` is the extension's package.json version; `tarballUrl`
// points at the gzipped tarball for that version and `sha256` is verified before
// extraction.
export interface BuiltinExtensionManifest {
  schemaVersion: 1;
  version: string;
  id: string;
  tarballUrl: string;
  sha256: string;
  signature: string;
}

export type SidecarKind = "llama" | "llama-embed" | "speech" | "tool";

export type SidecarStatus = "Disabled" | "Loading" | "Running" | "Error";

export interface SidecarSnapshot {
  kind: SidecarKind;
  status: SidecarStatus;
  pid?: number;
  rssMb?: number;
  cpuPct?: number;
  message?: string;
  // 0..1 progress hint for the load phase (e.g. model download). Absent
  // once running.
  progress?: number;
}
