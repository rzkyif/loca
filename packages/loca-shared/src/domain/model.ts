// Model + binary types shared across core and client.

// All triples the binary manifest publishes.
export const TRIPLES = [
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "aarch64-pc-windows-msvc",
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
] as const;

export type Triple = (typeof TRIPLES)[number];

export const BINARY_KINDS = ["llama-server", "deno"] as const;

export type BinaryKind = (typeof BINARY_KINDS)[number];

// GPU-backend build variants a binary may ship. `cpu` is the guaranteed
// fallback present for every triple; a device picks the best variant its
// hardware supports and the manifest offers, always terminating at `cpu`.
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

// HF-style spec: "@user/repo/branch/file". The storage path strips the leading
// "@" and is relative to ~/.loca/core/models/.
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
   *  retry note instead of a size. */
  error?: string;
}

// A binary manifest entry is one of two shapes:
//
//  - Pinned (stable channel): exact per-triple URLs + sha256, resolved at
//    release time. The signed manifest commits to the exact bytes.
//  - Resolver (latest channel): an upstream GitHub repo + per-triple asset name
//    patterns, resolved to the LATEST release at runtime and verified against
//    GitHub's published sha256 digest.

// One downloadable artifact: a primary asset-name pattern plus optional
// companion archives (e.g. the Windows CUDA `cudart` runtime) extracted into the
// same lib dir. "{tag}" expands to the release tag_name in every pattern.
export interface VariantAsset {
  asset: string;
  extra?: string[];
}

// Either a single bare asset pattern (single-variant, treated as `cpu`) or a
// per-variant map, which MUST include `cpu`. Normalize via `assetVariants`.
export type TripleAsset = string | Partial<Record<BinaryVariant, string | VariantAsset>>;

// One resolved, pinned download plus any companion archives (extracted
// libs-only into bin/lib/<kind>).
export interface PinnedTarget {
  url: string;
  sha256: string;
  extras?: { url: string; sha256: string }[];
}

// Pinned mirror of TripleAsset. Normalize via `platformVariants`.
export type VariantPlatform = PinnedTarget | Partial<Record<BinaryVariant, PinnedTarget>>;

export interface BinaryManifestPinnedEntry {
  version: string;
  platforms: Record<Triple, VariantPlatform>;
}

export interface UpstreamResolver {
  repo: string;
  assets: Partial<Record<Triple, TripleAsset>>;
  /** Resolve this exact release tag instead of the latest, on EVERY channel. */
  pinnedTag?: string;
}

/** Normalize a triple's asset config into a per-variant map, so single-variant
 *  kinds (deno, mac llama) need no special case downstream. */
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

/** Pinned mirror of {@link assetVariants}; a bare `PinnedTarget` is `cpu`. */
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
// release's tag_name). The release script pins it for stable and embeds it
// verbatim for latest; a dev core builds resolver entries from it in-code.
export const UPSTREAM_BINARIES: Partial<Record<BinaryKind, UpstreamResolver>> = {
  "llama-server": {
    repo: "ggml-org/llama.cpp",
    // macOS bakes Metal into the single default build, so mac stays a bare
    // (cpu-keyed) string.
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
    // Pinned on every channel: tool-worker permission prompts are parsed from
    // this version's wording. Re-run prompt-live-probe.test.ts before bumping.
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
  // Monotonic anchor used to refuse a strictly-older (replayed) manifest.
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
  /** What the last install AIMED for. Present-ness is judged by
   *  `target === desiredVariant`, so an install that degraded to an available
   *  variant settles instead of reading as perpetually-missing. */
  target?: BinaryVariant;
  /** The ideal variant for this device (detected hardware + backend override). */
  desiredVariant?: BinaryVariant;
}

// Resolved download metadata for a not-yet-installed binary. `version` is
// "unknown" when it couldn't be resolved.
export interface BinaryProbeResult {
  kind: BinaryKind;
  version: string;
  sizeBytes?: number;
}

// --- requirements -----------------------------------------------------------
//
// What the current configuration needs on disk (models + sidecar binaries). The
// core computes a RequirementsSnapshot and broadcasts it; the client renders one
// pending-downloads popup.

/** Synthetic source prefix for a sidecar binary surfaced in the same list as
 *  HF model files (e.g. `binary:llama-server`). */
export const BINARY_SOURCE_PREFIX = "binary:";

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
  /** Preparing this file failed and it needs a retry. Still counted in
   *  `missing` (it blocks), unlike `unavailable` (platform-impossible). */
  error?: string;
  sizeHint?: number;
  version?: string;
}

export interface RequirementsSnapshot {
  required: RequiredFile[];
  /** Subset of `required` that is not present and not unavailable. */
  missing: RequiredFile[];
}

export interface RequiredModelRef {
  source: string;
  group: RequirementGroup;
}

// The schema defaults for the two enable flags, repeated here so a SPARSE
// settings object (only non-default values on disk) yields the same answer as a
// resolved one.
const STT_ENABLED_DEFAULT = false;
const TTS_ENABLED_DEFAULT = false;

/** True when speech-to-text runs on the local engine. */
export function sttUsesLocal(s: Record<string, unknown>): boolean {
  const enabled = s["stt.enabled"] ?? STT_ENABLED_DEFAULT;
  return !!enabled && s["stt.provider"] !== "external";
}

/** Mirror of {@link sttUsesLocal} for the TTS gates. */
export function ttsUsesLocal(s: Record<string, unknown>): boolean {
  const enabled = s["tts.enabled"] ?? TTS_ENABLED_DEFAULT;
  return !!enabled && s["tts.provider"] !== "external";
}

/** True when `kind` has no upstream asset for `triple` (so it's marked
 *  `unavailable` rather than perpetually `missing`). */
export function binaryUnavailableOnTriple(kind: BinaryKind, triple: Triple): boolean {
  const resolver = UPSTREAM_BINARIES[kind];
  return !!resolver && Object.keys(assetVariants(resolver.assets[triple])).length === 0;
}

// Core update manifest (separate from binaries). `workers` carries the
// platform-independent tool-worker .ts; `helpers` carries per-triple binaries
// shipped next to core (loca-core-updater, which performs the swap).
export interface CoreManifest {
  schemaVersion: 1;
  version: string;
  binaries: Array<{ triple: Triple; url: string; sha256: string }>;
  workers: Array<{ name: string; url: string; sha256: string }>;
  helpers: Array<{ name: string; triple: Triple; url: string; sha256: string }>;
  signature: string;
}

// Signed manifest for the CDN-distributed built-in extension. Mirrors
// CoreManifest: the object minus `signature` is canonicalized + Ed25519-signed.
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
  // 0..1 progress hint for the load phase. Absent once running.
  progress?: number;
}
