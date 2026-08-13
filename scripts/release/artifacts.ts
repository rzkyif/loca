// The serializable contract between a build environment and the host.
//
// A full all-targets release builds each platform NATIVELY in its own
// environment (the host, a Podman Linux container, the Windows VM), because the
// speech sidecar (static sherpa-onnx) and the Tauri installers can't be
// cross-compiled. Each environment is a dumb artifact producer: it builds into
// dist/<triple>/… and emits a bundle.json describing what it produced. The host
// collects every environment's dist subtree, reads the bundles, and composes +
// signs + uploads the unified manifests ONCE (composeCoreManifest et al. carry
// no R2 carry-forward, so the host must hold the full union before composing).
//
// Paths in a record are RELATIVE to dist/ so they survive the move from an
// environment's filesystem to the host's dist/ (the layout is identical on both
// sides). The host re-anchors each relPath under its own DIST_DIR.

import { dirname, join } from "@std/path";
import { ensureDir } from "@std/fs/ensure-dir";
import type { BinaryVariant, Triple } from "../../packages/loca-shared/src/domain/model.ts";
import type {
  BuildArtifact,
  CoreBuildArtifacts,
  HelperArtifact,
  SpeechArtifact,
  WorkerArtifact,
} from "./core.ts";
import { DIST_DIR, fail, sha256File } from "./lib.ts";
import type { ReleaseChannel } from "./lib.ts";
import type { InstallerAsset } from "./core-installers.ts";

export const BUNDLE_FILENAME = "bundle.json";

// One produced file: its kind, the metadata the host's compose step needs, and
// a dist-relative path to the bytes. `sha256` mirrors the producing build (over
// the decompressed binary for core/helper, over the .tar.gz archive for speech),
// so the host never re-hashes; it re-verifies after copying instead.
export interface ArtifactRecord {
  kind: "core" | "helper" | "speech";
  triple: Triple;
  filename: string;
  relPath: string; // relative to dist/, e.g. "aarch64-apple-darwin/loca-core"
  sha256: string;
  size: number;
  entryName?: string; // helpers only: the channel-suffixed manifest name
  variant?: BinaryVariant; // speech only: the GPU build variant (cpu default)
}

// Everything one environment built for one set of triples. `workers` are
// platform-independent, so they are NOT carried here: the host hashes them from
// its own checkout when merging.
export interface ArtifactBundle {
  version: string;
  channel: ReleaseChannel;
  triples: Triple[];
  records: ArtifactRecord[];
  // Native installers this environment built, if any: the unified client+core
  // installer (Windows .exe / macOS .pkg) and/or the Linux Core packages
  // (deb/rpm). Auxiliary download assets; the publish coordinator uploads them
  // + a signed installers.json. Kept separate from `records` (which feed
  // core.json + self-update).
  installers?: InstallerAsset[];
}

/** dist-relative path for a `dist/<triple>/<filename>` artifact. */
function distRel(triple: Triple, filename: string): string {
  return `${triple}/${filename}`;
}

/** Describe an environment's freshly-built core artifacts as a serializable
 *  bundle. Workers are dropped (host re-hashes them); everything else maps to a
 *  record whose relPath points into dist/<triple>/. */
export function bundleCoreArtifacts(
  built: CoreBuildArtifacts,
  version: string,
  channel: ReleaseChannel,
  triples: Triple[],
): ArtifactBundle {
  const records: ArtifactRecord[] = [];
  for (const a of built.artifacts) {
    records.push({
      kind: "core",
      triple: a.triple,
      filename: a.filename,
      relPath: distRel(a.triple, a.filename),
      sha256: a.sha256,
      size: a.size,
    });
  }
  for (const h of built.helpers) {
    records.push({
      kind: "helper",
      triple: h.triple,
      filename: h.filename,
      relPath: distRel(h.triple, h.filename),
      sha256: h.sha256,
      size: h.size,
      entryName: h.entryName,
    });
  }
  for (const s of built.speech) {
    records.push({
      kind: "speech",
      triple: s.triple,
      filename: s.filename,
      relPath: distRel(s.triple, s.filename),
      sha256: s.sha256,
      size: s.size,
      variant: s.variant,
    });
  }
  return { version, channel, triples, records };
}

export async function writeBundle(stagingDir: string, bundle: ArtifactBundle): Promise<string> {
  const path = join(stagingDir, BUNDLE_FILENAME);
  await Deno.writeTextFile(path, JSON.stringify(bundle, null, 2));
  return path;
}

export async function readBundle(stagingDir: string): Promise<ArtifactBundle> {
  const path = join(stagingDir, BUNDLE_FILENAME);
  const text = await Deno.readTextFile(path).catch(() => fail(`no ${BUNDLE_FILENAME} in ${path}`));
  return JSON.parse(text) as ArtifactBundle;
}

/** Re-anchor a dist-relative path onto the host's dist/ and re-verify its bytes
 *  against the sha256 the producing environment committed to. A mismatch means
 *  the artifact was corrupted or swapped in transit between environment and host.
 *  Shared by the core bundle merge and the client/android descriptor publish. */
export async function reanchorFile(relPath: string, expectedSha256: string): Promise<string> {
  const path = join(DIST_DIR, relPath);
  const { sha256 } = await sha256File(path);
  if (sha256 !== expectedSha256) {
    fail(
      `artifact sha256 mismatch for ${relPath}: expected ${expectedSha256}, host ${sha256}. ` +
        `The file changed between its build environment and the host.`,
    );
  }
  return path;
}

function reanchor(rec: ArtifactRecord): Promise<string> {
  return reanchorFile(rec.relPath, rec.sha256);
}

/** Merge bundles from every environment (their files already collected under the
 *  host's dist/) plus the host-hashed workers into the single CoreBuildArtifacts
 *  the host's composeAndUploadCore consumes. Re-verifies every artifact's hash. */
export async function mergeCoreBundles(
  bundles: ArtifactBundle[],
  workers: WorkerArtifact[],
): Promise<CoreBuildArtifacts> {
  const artifacts: BuildArtifact[] = [];
  const helpers: HelperArtifact[] = [];
  const speech: SpeechArtifact[] = [];
  for (const bundle of bundles) {
    for (const rec of bundle.records) {
      const path = await reanchor(rec);
      if (rec.kind === "core") {
        artifacts.push({
          triple: rec.triple,
          name: "loca-core",
          filename: rec.filename,
          path,
          sha256: rec.sha256,
          size: rec.size,
        });
      } else if (rec.kind === "helper") {
        helpers.push({
          triple: rec.triple,
          entryName: rec.entryName ?? fail(`helper record missing entryName: ${rec.relPath}`),
          filename: rec.filename,
          path,
          sha256: rec.sha256,
          size: rec.size,
        });
      } else {
        speech.push({
          triple: rec.triple,
          variant: rec.variant ?? "cpu",
          filename: rec.filename,
          path,
          sha256: rec.sha256,
          size: rec.size,
        });
      }
    }
  }
  return { artifacts, helpers, speech, workers };
}

// ---------------------------------------------------------------------------
// client + android descriptors
//
// The desktop client and the Android APK can't be cross-compiled, so each is
// built (and build-time-signed: Tauri minisign for the bundle, the Java keystore
// for the APK) in its own native environment, exactly like core. A descriptor is
// that environment's serializable hand-off: it names the produced file(s) by a
// dist-relative path (the build copies them under dist/ so they survive the move)
// plus the metadata the host's compose+sign+upload step needs. Mirrors the core
// ArtifactBundle, one module so drivers and CI share the contract.

/** One desktop-client bundle built on its native platform. `signature` is the
 *  Tauri minisign signature over the bundle (for in-app updates); `sha256` is
 *  over the bundle bytes (for first-install verification, mirrors core.json). */
export interface ClientDescriptor {
  version: string;
  channel: ReleaseChannel;
  triple: Triple;
  /** Tauri updater platform key (<os>-<arch>), e.g. "darwin-aarch64". */
  tauriKey: string;
  filename: string;
  relPath: string; // dist-relative, e.g. "aarch64-apple-darwin/Loca.app.tar.gz"
  sigRelPath: string; // dist-relative path to the Tauri .sig sidecar
  sha256: string;
  size: number;
  signature: string;
}

/** One keystore-signed APK for a single ABI. */
export interface AndroidApkRecord {
  abi: string; // android.json key, e.g. "android-arm64"
  filename: string;
  relPath: string; // dist-relative, e.g. "android/android-arm64/loca.apk"
  sha256: string;
  size: number;
}

/** Everything one Android build produced (all ABIs from a single Gradle run). */
export interface AndroidDescriptor {
  version: string;
  channel: ReleaseChannel;
  apks: AndroidApkRecord[];
}

/** Pre-built artifacts collected from every build environment, ready for the
 *  host's single compose+sign+upload pass. Populated by the CI publish step from
 *  the per-runner staging dirs; absent for a local single-host build. */
export interface PrebuiltStaging {
  coreBundles: ArtifactBundle[];
  clientDescriptors: ClientDescriptor[];
  android?: AndroidDescriptor;
}

export const CLIENT_DESCRIPTOR_FILENAME = "client-descriptor.json";
export const ANDROID_DESCRIPTOR_FILENAME = "android-descriptor.json";

export async function writeClientDescriptor(
  stagingDir: string,
  descriptor: ClientDescriptor,
): Promise<string> {
  const path = join(stagingDir, CLIENT_DESCRIPTOR_FILENAME);
  await Deno.writeTextFile(path, JSON.stringify(descriptor, null, 2));
  return path;
}

export async function readClientDescriptor(stagingDir: string): Promise<ClientDescriptor> {
  const path = join(stagingDir, CLIENT_DESCRIPTOR_FILENAME);
  return JSON.parse(await Deno.readTextFile(path)) as ClientDescriptor;
}

export async function writeAndroidDescriptor(
  stagingDir: string,
  descriptor: AndroidDescriptor,
): Promise<string> {
  const path = join(stagingDir, ANDROID_DESCRIPTOR_FILENAME);
  await Deno.writeTextFile(path, JSON.stringify(descriptor, null, 2));
  return path;
}

export async function readAndroidDescriptor(stagingDir: string): Promise<AndroidDescriptor> {
  const path = join(stagingDir, ANDROID_DESCRIPTOR_FILENAME);
  return JSON.parse(await Deno.readTextFile(path)) as AndroidDescriptor;
}

/** Copy one dist-relative artifact into a staging dir's mirrored `dist/` tree,
 *  so a CI build runner can upload exactly the files its descriptors reference
 *  (not the whole dist/). The host re-anchors them under its own DIST_DIR. */
export async function stageDistFile(stagingDir: string, relPath: string): Promise<void> {
  const src = join(DIST_DIR, relPath);
  const dest = join(stagingDir, "dist", relPath);
  await ensureDir(dirname(dest));
  await Deno.copyFile(src, dest);
}
