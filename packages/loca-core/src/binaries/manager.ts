// Binaries manager: download + extract + install the platform-specific
// archive for each binary kind referenced by the runtime-fetched manifest.
//
// Two archive shapes are supported, picked by the asset's extension:
//   .tar.gz  (llama.cpp upstream; loca-core-speech): the executable named
//            `<kind>(.exe)` plus any shared libraries (.so/.dylib), located by
//            basename wherever they sit. loca-core-speech additionally carries
//            an `espeak-ng-data/` tree, preserved verbatim under bin/lib/<kind>/.
//   .zip     (deno, Windows llama): lenient layout -
//            the executable (`<kind>(.exe)`) and any shared libraries
//            (.so/.dylib/.dll) are located wherever they sit in the archive.
//
// Either way the executable lands at bin/<kind>(.exe) and shared libs at
// bin/lib/<kind>/ (the dir each sidecar is launched with as its library path).

import { UntarStream } from "@std/tar/untar-stream";
import { configure, Uint8ArrayReader, Uint8ArrayWriter, ZipReader } from "@zip.js/zip.js";
import { dirname, join } from "@std/path";
import type {
  BinaryKind,
  BinaryManifest,
  BinaryManifestEntry,
  BinaryProbeResult,
  BinaryStatus,
  BinaryVariant,
  GpuBackend,
  Triple,
} from "@loca/shared";
import {
  assetVariants,
  BINARY_KINDS,
  errMessage,
  isResolverEntry,
  platformVariants,
  selectVariant,
  variantPreference,
} from "@loca/shared";
import { downloadManager } from "../downloads/manager.ts";
import { paths } from "../paths.ts";
import { AppError } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { newJobId } from "@loca/core-engine";
import { loadCoreSettingsResolved } from "@loca/core-engine/services/core-settings";
import { detectHardware } from "../models/hardware.ts";
import { loadBinaryManifest } from "./manifest.ts";
import { binaryName, hostTriple, libDirFor } from "./versions.ts";
import { resolveBinaryEntry, type ResolvedBinary } from "./upstream-resolver.ts";

// Which resolved-settings key overrides the auto-detected GPU backend for each
// kind. "auto" (or blank/absent) means "use detected hardware"; a concrete
// variant forces that build (falling back to cpu if the platform doesn't offer
// it). deno has no GPU variants, so it has no override. See the settings groups.
const BACKEND_OVERRIDE_KEY: Partial<Record<BinaryKind, string>> = {
  "llama-server": "llm.binaryBackend",
  "loca-core-speech": "speech.binaryBackend",
};

/** The set of variant keys this entry offers for `triple` (from either the
 *  resolver asset map or the pinned platform map). `cpu` is always present for
 *  a covered triple. */
function offeredVariants(
  entry: BinaryManifestEntry,
  triple: Triple,
): Partial<Record<BinaryVariant, unknown>> {
  return isResolverEntry(entry)
    ? assetVariants(entry.resolver.assets[triple])
    : platformVariants(entry.platforms[triple]);
}

/** The variant this device should install for `kind`: the settings override
 *  when set to a concrete offered variant, otherwise the best offered variant
 *  for the detected backend. Always resolves to something `offered` contains
 *  (falling back to `cpu`). */
function desiredVariant(
  entry: BinaryManifestEntry,
  triple: Triple,
  kind: BinaryKind,
  backend: GpuBackend,
  settings: Record<string, unknown>,
): BinaryVariant {
  const offered = offeredVariants(entry, triple);
  const overrideKey = BACKEND_OVERRIDE_KEY[kind];
  const override = overrideKey ? settings[overrideKey] : undefined;
  if (typeof override === "string" && override !== "auto" && override !== "") {
    return offered[override as BinaryVariant] !== undefined ? (override as BinaryVariant) : "cpu";
  }
  return selectVariant(offered, backend);
}

/** Resolve `entry` for `triple`, preferring `target` but DEGRADING to the
 *  next-best offered variant (in the detected backend's preference order, always
 *  ending at cpu) when the preferred one can't be resolved -- e.g. a renamed or
 *  removed upstream GPU asset on the latest channel. So a device that wants cuda
 *  but whose cuda asset vanished still gets a working vulkan/cpu build instead of
 *  a failed install. On the stable channel `offered` is the pinned set (every
 *  entry already resolvable), so the target resolves on the first try and this
 *  never degrades. Returns the resolved best-installable variant, or null when
 *  nothing (not even cpu) is offered for the triple; throws only when every
 *  candidate errored (e.g. the network is down). Exported for testing. */
export async function resolveWithFallback(
  kind: BinaryKind,
  entry: BinaryManifestEntry,
  triple: Triple,
  target: BinaryVariant,
  backend: GpuBackend,
): Promise<ResolvedBinary | null> {
  const offered = offeredVariants(entry, triple);
  // Candidate order: the target first, then the remaining preference order
  // (offered only), always ending at cpu (the guaranteed fallback).
  const candidates: BinaryVariant[] = [];
  const add = (v: BinaryVariant) => {
    if (offered[v] !== undefined && !candidates.includes(v)) candidates.push(v);
  };
  add(target);
  for (const v of variantPreference(backend)) add(v);
  add("cpu");
  let lastErr: unknown;
  for (const v of candidates) {
    try {
      const resolved = await resolveBinaryEntry(entry, triple, v);
      if (resolved) {
        if (v !== target) {
          log.warn(`${kind}: ${target} variant unavailable upstream; falling back to ${v}`);
        }
        return resolved;
      }
    } catch (err) {
      // A per-variant resolve failure (missing asset/digest, or a network error)
      // is not fatal on its own: try the next candidate. Only if EVERY candidate
      // fails do we surface the last error to the caller.
      lastErr = err;
      log.warn(`${kind}: resolving ${v} for ${triple} failed: ${errMessage(err)}`);
    }
  }
  if (lastErr) throw lastErr;
  return null;
}

const log = getLogger("binaries");

// zip-js spins up Web Workers by default; Deno's worker model differs, so run
// inline (binaries are tens of MB - acceptable to decode on the main thread).
configure({ useWebWorkers: false });

export interface InstallResult {
  jobIds: string[];
}

// Notified after a binary is fully installed (downloaded AND extracted to
// bin/). Distinct from the download-completion broadcast, which fires on the
// .tar.gz finishing, before extraction. `sidecar-boot` listens here so a
// sidecar left Disabled for a missing binary can start once it lands.
type BinaryInstalledListener = (kind: BinaryKind) => void;
const binaryInstalledListeners = new Set<BinaryInstalledListener>();

export function onBinaryInstalled(fn: BinaryInstalledListener): () => void {
  binaryInstalledListeners.add(fn);
  return () => binaryInstalledListeners.delete(fn);
}

// Notified when a binary install ATTEMPT fails (couldn't resolve upstream, the
// download errored, or extraction failed). The requirements layer recomputes on
// this so a failed kind surfaces as a retryable `error` in the pending list
// instead of a silent, sizeless, permanently-missing entry.
type BinaryInstallFailedListener = (kind: BinaryKind) => void;
const binaryInstallFailedListeners = new Set<BinaryInstallFailedListener>();

export function onBinaryInstallFailed(fn: BinaryInstallFailedListener): () => void {
  binaryInstallFailedListeners.add(fn);
  return () => binaryInstallFailedListeners.delete(fn);
}

interface InstalledRecord {
  version: string;
  /** The variant actually installed on disk. */
  variant: BinaryVariant;
  /** The desired variant we AIMED for at install time. Differs from `variant`
   *  only when that ideal wasn't resolvable upstream and we degraded (latest
   *  channel: a renamed/removed GPU asset). The present/stale check compares the
   *  CURRENT desired against this, network-free, so a permanent, unavoidable
   *  fallback SETTLES instead of looping the download popup; check() (consented
   *  network) still upgrades if the ideal later becomes available. */
  target: BinaryVariant;
}

/** Per-binary installed-version + variant index. Updated on successful install /
 *  update so /api/v1/binaries/check can report what's actually on disk (version
 *  AND GPU variant) vs what the manifest + detected hardware call for. A legacy
 *  bare-string value (version only, pre-variant) normalizes to the `cpu`
 *  variant. */
async function readInstalled(): Promise<Record<string, InstalledRecord>> {
  try {
    const text = await Deno.readTextFile(join(paths().binDir, "versions.json"));
    const parsed = JSON.parse(text);
    if (parsed && typeof parsed === "object") {
      const out: Record<string, InstalledRecord> = {};
      for (const [kind, val] of Object.entries(parsed as Record<string, unknown>)) {
        if (typeof val === "string") {
          // Legacy bare-string value (version only, pre-variant): cpu, and its
          // own variant is the target it aimed for.
          out[kind] = { version: val, variant: "cpu", target: "cpu" };
        } else if (
          val &&
          typeof val === "object" &&
          typeof (val as InstalledRecord).version === "string"
        ) {
          const rec = val as Partial<InstalledRecord> & { version: string };
          const variant = rec.variant ?? "cpu";
          // A record written before `target` existed aimed for exactly what it
          // installed, so default target to that variant.
          out[kind] = { version: rec.version, variant, target: rec.target ?? variant };
        }
      }
      return out;
    }
  } catch (err) {
    if (!(err instanceof Deno.errors.NotFound)) {
      log.warn(`could not read versions.json: ${errMessage(err)}`);
    }
  }
  return {};
}

async function writeInstalledVersion(
  kind: BinaryKind,
  version: string,
  variant: BinaryVariant,
  target: BinaryVariant,
): Promise<void> {
  const versions = await readInstalled();
  versions[kind] = { version, variant, target };
  await Deno.writeTextFile(
    join(paths().binDir, "versions.json"),
    JSON.stringify(versions, null, 2) + "\n",
  );
}

export interface BinaryUpdateCheck {
  kind: BinaryKind;
  installed: boolean;
  installedVersion: string | null;
  latestVersion: string;
  available: boolean;
}

/** The GPU variant currently installed on disk for `kind` (from versions.json),
 *  or null when the binary isn't installed. Lets the speech sidecar pick the
 *  ONNX execution provider that matches the binary it will actually launch. */
export async function installedVariant(kind: BinaryKind): Promise<BinaryVariant | null> {
  const versions = await readInstalled();
  return versions[kind]?.variant ?? null;
}

export class BinariesManager {
  // Last install-attempt error per kind, cleared when a fresh attempt starts or
  // succeeds. Read by computeRequirements to mark a missing binary as retryable.
  private readonly failures = new Map<BinaryKind, string>();

  /** The reason the last install attempt for `kind` failed, or undefined when
   *  the most recent attempt is pending or succeeded. */
  failure(kind: BinaryKind): string | undefined {
    return this.failures.get(kind);
  }

  private recordFailure(kind: BinaryKind, message: string): void {
    this.failures.set(kind, message);
    for (const fn of binaryInstallFailedListeners) {
      try {
        fn(kind);
      } catch (err) {
        log.warn(`binary-install-failed listener for ${kind}: ${errMessage(err)}`);
      }
    }
  }

  // List the install state of every known binary kind. Used by
  // GET /api/v1/binaries.
  async list(): Promise<BinaryStatus[]> {
    const manifest = await loadBinaryManifest().catch(() => null);
    const versions = await readInstalled();
    const triple = hostTriple();
    // Settings (override) + hardware (detected backend) drive the desired
    // variant. Both are local (file read + in-process-cached probe), so a plain
    // list still never hits the network.
    const settings = await loadCoreSettingsResolved().catch(() => ({}) as Record<string, unknown>);
    const backend = (await detectHardware().catch(() => null))?.gpu.backend ?? "cpu";
    const out: BinaryStatus[] = [];
    for (const kind of BINARY_KINDS) {
      const path = join(paths().binDir, binaryName(kind));
      const installed = await fileExists(path);
      const entry = manifest?.binaries[kind];
      // Don't hit GitHub for a plain list: resolver (latest) entries report the
      // installed version (or "latest" if not yet installed); pinned (stable)
      // entries report their published version.
      const version = entry
        ? isResolverEntry(entry)
          ? (versions[kind]?.version ?? "latest")
          : entry.version
        : "unknown";
      out.push({
        kind,
        version,
        installed,
        path: installed ? path : undefined,
        variant: installed ? versions[kind]?.variant : undefined,
        target: installed ? versions[kind]?.target : undefined,
        desiredVariant: entry ? desiredVariant(entry, triple, kind, backend, settings) : undefined,
      });
    }
    return out;
  }

  // Compare installed binary versions to the manifest's currently-published
  // version. Drives the UpdateButton's "updates available" affordance.
  async check(): Promise<BinaryUpdateCheck[]> {
    const manifest = await loadBinaryManifest({ force: true });
    const versions = await readInstalled();
    const triple = hostTriple();
    const settings = await loadCoreSettingsResolved().catch(() => ({}) as Record<string, unknown>);
    const backend = (await detectHardware().catch(() => null))?.gpu.backend ?? "cpu";
    const out: BinaryUpdateCheck[] = [];
    for (const kind of BINARY_KINDS) {
      const path = join(paths().binDir, binaryName(kind));
      const installed = await fileExists(path);
      const installedRec = versions[kind] ?? null;
      const installedVersion = installedRec?.version ?? null;
      // Resolve the latest available version for this host + desired variant.
      // For latest resolver entries this hits GitHub; a per-kind failure
      // (missing asset/digest) degrades to "no update" rather than failing the
      // whole check.
      let latestVersion = "unknown";
      let resolvable = false;
      // The variant that WOULD actually install: the desired one, or its
      // best-resolvable fallback. check() runs on the consented update path, so
      // it can afford the network to learn what's genuinely installable (unlike
      // the network-free present-check, which compares stored targets instead).
      let effectiveVariant: BinaryVariant = "cpu";
      const entry = manifest.binaries[kind];
      if (entry) {
        const wantTarget = desiredVariant(entry, triple, kind, backend, settings);
        try {
          const resolved = await resolveWithFallback(kind, entry, triple, wantTarget, backend);
          if (resolved) {
            latestVersion = resolved.version;
            effectiveVariant = resolved.variant;
            resolvable = true;
          }
        } catch (err) {
          log.warn(`check ${kind}: resolve failed: ${errMessage(err)}`);
        }
      }
      // "available" means we should (re)install: the binary is missing, the
      // recorded install version disagrees with the latest, OR the installed GPU
      // variant differs from the best one now installable (e.g. a GPU appeared,
      // the override changed, or a better upstream asset became available since a
      // prior fallback). Comparing against the RESOLVED variant (not the ideal
      // desired) means a still-unavailable GPU asset never shows a phantom update.
      // Guarded on `resolvable` so a triple with no upstream asset never does
      // either. An installed-but-unknown version (pre-tracking) is treated as
      // current for the version check.
      const versionStale = installedVersion !== null && installedVersion !== latestVersion;
      const variantStale = installedRec !== null && installedRec.variant !== effectiveVariant;
      const available = resolvable && (!installed || versionStale || variantStale);
      out.push({
        kind,
        installed,
        installedVersion,
        latestVersion,
        available,
      });
    }
    return out;
  }

  // Resolve the version + download size for each requested kind, without
  // installing. Backs the startup download confirmation so it can show the
  // concrete release and size before the user commits. Per-kind failures
  // degrade to version "unknown" / no size rather than failing the whole probe.
  async probe(kinds: BinaryKind[]): Promise<BinaryProbeResult[]> {
    const manifest = await loadBinaryManifest().catch(() => null);
    const triple = hostTriple();
    const settings = await loadCoreSettingsResolved().catch(() => ({}) as Record<string, unknown>);
    const backend = (await detectHardware().catch(() => null))?.gpu.backend ?? "cpu";
    const out: BinaryProbeResult[] = [];
    for (const kind of kinds) {
      const entry = manifest?.binaries[kind];
      if (!entry) {
        out.push({ kind, version: "unknown" });
        continue;
      }
      try {
        const target = desiredVariant(entry, triple, kind, backend, settings);
        // Size the variant that will actually install (target or its fallback),
        // so the download-confirm popup shows the real size.
        const resolved = await resolveWithFallback(kind, entry, triple, target, backend);
        if (!resolved) {
          out.push({ kind, version: "unknown" });
          continue;
        }
        // Pinned (stable) entries don't carry a size; fall back to a HEAD on
        // the resolved URL. Resolver (latest) entries already know it.
        const sizeBytes = resolved.sizeBytes ?? (await headContentLength(resolved.url));
        out.push({ kind, version: resolved.version, sizeBytes });
      } catch (err) {
        log.warn(`probe ${kind}: resolve failed: ${errMessage(err)}`);
        out.push({ kind, version: "unknown" });
      }
    }
    return out;
  }

  // Install (or replace) the named binaries. If `kinds` is omitted, installs
  // every kind not currently present. Returns one jobId per kicked-off
  // download; progress streams over the WS via downloads.snapshot.
  async install(kinds?: BinaryKind[]): Promise<InstallResult> {
    const manifest = await loadBinaryManifest();
    const triple = hostTriple();
    const targets = kinds ?? (await this.missingKinds());
    const jobIds: string[] = [];
    for (const kind of targets) {
      // One kind failing to resolve (e.g. a renamed upstream GPU asset on the
      // latest channel, so kickoff's pre-download resolve throws) must not block
      // the others: log and continue so the remaining binaries still install.
      // The failed kind stays missing and the requirements popup keeps offering
      // it. update() (single kind) still surfaces the throw to its caller.
      try {
        const job = await this.kickoff(kind, manifest, triple);
        jobIds.push(job);
      } catch (err) {
        log.error(`install ${kind}: ${errMessage(err)}`);
      }
    }
    return { jobIds };
  }

  // Force-update a single binary to the manifest's currently-published
  // version. No client-side version pinning: the CDN publishes one version
  // per kind at a time, and that's what we install.
  async update(kind: BinaryKind): Promise<{ jobId: string }> {
    const manifest = await loadBinaryManifest({ force: true });
    const triple = hostTriple();
    const jobId = await this.kickoff(kind, manifest, triple);
    return { jobId };
  }

  /** Finish binary installs interrupted by a process exit before their archive
   *  was extracted. The usual cause is the in-app update flow: `runInstall`
   *  kicks off sidecar downloads and then triggers the core self-update, whose
   *  `Deno.exit(0)` (handoff to the updater) tears the process down mid-download.
   *
   *  The generic download manager can resume the byte transfer, but the
   *  extract-into-binDir + `writeInstalledVersion` step lives only in kickoff's
   *  in-process closure, which dies with the old process. A resumed transfer
   *  would therefore land the archive in staging and never install it (and its
   *  sha256 isn't re-verified on a generic resume either). So the download
   *  manager deliberately skips `binaries` rows in `resumePending`, and here we
   *  re-run the full kickoff for each affected kind instead: it re-resolves the
   *  URL + sha256, re-downloads, verifies, extracts, and records the version
   *  through the normal path.
   *
   *  Called once from main() on boot, right after `resumePending`. Only touches
   *  interrupted `binaries` downloads (never models), reuses the cached manifest
   *  (no forced network unless there is interrupted binary work), and is
   *  best-effort: a per-kind failure is recorded as a retryable requirements
   *  gate and never blocks boot. */
  async reconcileInterruptedInstalls(): Promise<void> {
    const stale = downloadManager()
      .snapshot()
      .filter((r) => r.destination === "binaries" && r.status === "Pending");
    if (stale.length === 0) return;

    const kinds = new Set<BinaryKind>();
    for (const row of stale) {
      // kickoff tags every row (main + companion extras) with `binary:<kind>`.
      if (row.groupId.startsWith("binary:")) {
        const kind = row.groupId.slice("binary:".length);
        if ((BINARY_KINDS as readonly string[]).includes(kind)) {
          kinds.add(kind as BinaryKind);
        }
      }
      // Drop the orphaned row + partial: kickoff enqueues a fresh jobId, so the
      // stale row would otherwise linger forever with no worker (stranding the
      // corebar on "Downloading"). remove() no-ops on an in-flight row, but
      // resumePending skipped binaries so none is in flight at this point.
      downloadManager().remove(row.id);
    }
    if (kinds.size === 0) return;

    // Cache-first: the manifest was fetched + cached during the original
    // (consented) update, so re-initiating the interrupted download reuses it
    // with no new network. Only reached when interrupted binary work exists.
    const manifest = await loadBinaryManifest().catch(() => null);
    if (!manifest) {
      log.warn("reconcile: binary manifest unavailable; leaving interrupted installs for retry");
      return;
    }
    const triple = hostTriple();
    for (const kind of kinds) {
      try {
        await this.kickoff(kind, manifest, triple);
        log.info(`reconcile: re-initiated interrupted install of ${kind}`);
      } catch (err) {
        log.warn(`reconcile: re-initiate ${kind} failed: ${errMessage(err)}`);
      }
    }
  }

  private async missingKinds(): Promise<BinaryKind[]> {
    const out: BinaryKind[] = [];
    for (const kind of BINARY_KINDS) {
      const path = join(paths().binDir, binaryName(kind));
      if (!(await fileExists(path))) out.push(kind);
    }
    return out;
  }

  private async kickoff(
    kind: BinaryKind,
    manifest: BinaryManifest,
    triple: Triple,
  ): Promise<string> {
    // A fresh attempt clears any prior failure so this call doubles as retry.
    this.failures.delete(kind);
    let target: BinaryVariant;
    let resolved: ResolvedBinary;
    // The pre-download resolve can throw (missing manifest entry, no installable
    // variant, upstream not resolvable). Record it as a failure BEFORE rethrowing
    // so the requirements popup shows a retryable error rather than silently
    // dropping the kind (which stranded llama-server with no size, no download).
    try {
      const entry = manifest.binaries[kind];
      if (!entry) {
        throw new AppError("binary_not_found", `kind ${kind} not in manifest`);
      }
      const settings = await loadCoreSettingsResolved().catch(
        () => ({}) as Record<string, unknown>,
      );
      const backend = (await detectHardware().catch(() => null))?.gpu.backend ?? "cpu";
      // The ideal variant this device wants; `resolveWithFallback` degrades to the
      // best actually-installable one (target === resolved.variant in the normal
      // case, differing only on a latest-channel asset miss). We record BOTH: the
      // installed variant and the target aimed for.
      target = desiredVariant(entry, triple, kind, backend, settings);
      // Resolve to a concrete URL+hash+version. For pinned (stable) entries this is
      // the stored data; for resolver (latest) entries it hits GitHub for the
      // latest release and verifies via its published sha256 digest.
      const r = await resolveWithFallback(kind, entry, triple, target, backend);
      if (!r) {
        throw new AppError(
          "binary_not_found",
          `kind ${kind} has no installable variant for triple ${triple}`,
        );
      }
      resolved = r;
    } catch (err) {
      this.recordFailure(kind, errMessage(err));
      throw err;
    }
    const jobId = newJobId();
    const ext = resolved.url.toLowerCase().endsWith(".zip") ? "zip" : "tar.gz";
    const relPath = `staging/${kind}-${jobId}.${ext}`;

    // Run the download + extract in the background; the WS shows progress.
    void (async () => {
      // The download manager resolves relPath under binDir (destination
      // "binaries"), so the archive lands at binDir/staging/..., NOT
      // paths().stagingDir. Track the actual path so the failure-path cleanup
      // targets the real file instead of a non-existent stagingDir entry.
      let downloadedPath: string | undefined;
      const extraPaths: string[] = [];
      try {
        downloadedPath = await downloadManager().enqueue({
          source: resolved.url,
          url: resolved.url,
          relPath,
          destination: "binaries",
          groupId: `binary:${kind}`,
          sha256: resolved.sha256,
        });
        log.info(`${kind}: downloaded to ${downloadedPath}`);
        // Wipe the per-kind lib dir before extracting so a variant swap never
        // leaves stale backend libs behind. This is required on Windows, where
        // the sidecar's cwd is its lib dir and ggml_backend_load_all() scans it
        // for compute-backend plugin DLLs; a leftover DLL from the old variant
        // could crash or mis-select a backend.
        await Deno.remove(libDirFor(paths().binDir, kind), { recursive: true }).catch(() => {});
        await extractArchive(downloadedPath, paths().binDir, kind);
        // Companion archives (e.g. the Windows CUDA cudart runtime) carry only
        // shared libs; extract them into the same bin/lib/<kind> dir.
        for (let i = 0; i < (resolved.extras?.length ?? 0); i++) {
          const extra = resolved.extras![i];
          const extraExt = extra.url.toLowerCase().endsWith(".zip") ? "zip" : "tar.gz";
          const p = await downloadManager().enqueue({
            source: extra.url,
            url: extra.url,
            relPath: `staging/${kind}-extra${i}-${jobId}.${extraExt}`,
            destination: "binaries",
            groupId: `binary:${kind}`,
            sha256: extra.sha256,
          });
          extraPaths.push(p);
          await extractLibsOnly(p, paths().binDir, kind);
        }
        await Deno.remove(downloadedPath);
        downloadedPath = undefined; // removed; nothing left to clean up
        for (const p of extraPaths.splice(0)) await Deno.remove(p).catch(() => {});
        await writeInstalledVersion(kind, resolved.version, resolved.variant, target);
        this.failures.delete(kind);
        log.info(
          `${kind}: installed v${resolved.version} (${resolved.variant}` +
            `${resolved.variant !== target ? `, wanted ${target}` : ""})`,
        );
        for (const fn of binaryInstalledListeners) {
          try {
            fn(kind);
          } catch (err) {
            log.warn(`binary-installed listener for ${kind}: ${errMessage(err)}`);
          }
        }
      } catch (err) {
        // Download or extraction failed after a clean resolve. Record it so the
        // kind surfaces as retryable instead of a silent perpetually-missing gate.
        log.error(`${kind}: install failed: ${errMessage(err)}`);
        this.recordFailure(kind, errMessage(err));
      } finally {
        for (const p of [downloadedPath, ...extraPaths].filter((p): p is string => !!p)) {
          try {
            await Deno.remove(p);
          } catch {
            /* fine */
          }
        }
      }
    })();
    return jobId;
  }
}

let _instance: BinariesManager | null = null;
export function binariesManager(): BinariesManager {
  if (!_instance) _instance = new BinariesManager();
  return _instance;
}

export function __resetForTesting(): void {
  _instance = null;
}

// --- archive extraction ----------------------------------------------------

// Dispatch on the archive extension. `.zip` uses the lenient extractor; every
// other archive is treated as `.tar.gz`. Exported for testing.
export async function extractArchive(
  archivePath: string,
  targetDir: string,
  kind: BinaryKind,
): Promise<void> {
  if (archivePath.toLowerCase().endsWith(".zip")) {
    return await extractZip(archivePath, targetDir, kind);
  }
  return await extractTarGz(archivePath, targetDir, kind);
}

// Extracts a .tar.gz at `archivePath` into `targetDir`. Lenient, like the .zip
// path: the executable (`<kind>(.exe)`) and any shared libraries (.so/.dylib,
// including versioned SONAMEs) are located by BASENAME wherever they sit in the
// archive. Real release tarballs nest everything under a tag-named dir (e.g.
// `llama-bNNNN/llama-server`, `llama-bNNNN/libggml-base.so`) and ship many
// extra binaries + a LICENSE, so a strict "exe-at-root + lib/* only" layout
// would reject them. The exe lands at <targetDir>/<exeName>, libs at
// <targetDir>/lib/<kind>/<basename> (per-kind, no collisions); the SONAME
// symlink chain (libfoo.so -> libfoo.so.0 -> libfoo.so.0.X) is recreated so the
// dynamic loader resolves it; everything else is ignored.
async function extractTarGz(
  archivePath: string,
  targetDir: string,
  kind: BinaryKind,
): Promise<void> {
  const exeName = binaryName(kind);
  const libDir = libDirFor(targetDir, kind);
  await Deno.mkdir(libDir, { recursive: true });

  const file = await Deno.open(archivePath, { read: true });
  const gunzip = new DecompressionStream("gzip");
  const entries = file.readable.pipeThrough(gunzip).pipeThrough(new UntarStream());

  let exeFound = false;
  for await (const entry of entries) {
    const path = entry.path.replace(/^\.\//, "");
    const base = path.split("/").pop() ?? path;
    const flag = entry.header.typeflag;
    if (flag === "5" /* directory */) {
      await entry.readable?.cancel();
      continue;
    }
    if (flag === "2" /* symlink: the SONAME chain for shared libs */) {
      await entry.readable?.cancel();
      const linkname = (entry.header as { linkname?: string }).linkname ?? "";
      if (isSharedLib(base) && linkname) {
        const targetBase = linkname.split("/").pop() ?? linkname;
        const dest = join(libDir, base);
        await Deno.remove(dest).catch(() => {});
        await Deno.symlink(targetBase, dest).catch(() => {});
      }
      continue;
    }
    if (base === exeName) {
      await writeStreamTo(entry.readable, join(targetDir, exeName), 0o755);
      exeFound = true;
    } else if (isSharedLib(base)) {
      await writeStreamTo(entry.readable, join(libDir, base), 0o644);
    } else if (path === "espeak-ng-data" || path.startsWith("espeak-ng-data/")) {
      // loca-core-speech ships its espeak-ng-data phonemizer tree; preserve the
      // nested layout under bin/lib/<kind>/ (writeStreamTo won't mkdir parents).
      const dest = join(libDir, path);
      await Deno.mkdir(dirname(dest), { recursive: true });
      await writeStreamTo(entry.readable, dest, 0o644);
    } else {
      await entry.readable?.cancel(); // other binaries, licenses, READMEs
    }
  }
  if (!exeFound) {
    throw new AppError("extract_failed", `archive for ${kind} missing ${exeName}`);
  }
}

// Extracts a `.zip` at `archivePath` into `targetDir`, locating the executable
// (`<kind>(.exe)`) and any shared libraries (.so/.dylib/.dll) wherever they sit
// in the archive: the exe lands at <targetDir>/<exeName>, libs at
// <targetDir>/lib/<kind>/<basename> (per-kind so same-named libs from different
// sidecars never collide). Tolerant of extra files (READMEs, licenses).
async function extractZip(archivePath: string, targetDir: string, kind: BinaryKind): Promise<void> {
  const exeName = binaryName(kind);
  const libDir = libDirFor(targetDir, kind);
  await Deno.mkdir(libDir, { recursive: true });

  const bytes = await Deno.readFile(archivePath);
  const reader = new ZipReader(new Uint8ArrayReader(bytes));
  let exeFound = false;
  try {
    for (const entry of await reader.getEntries()) {
      if (entry.directory || !entry.getData) continue;
      const base = entry.filename.split("/").pop() ?? entry.filename;
      if (base === exeName) {
        const data = await entry.getData(new Uint8ArrayWriter());
        await writeBytesTo(data, join(targetDir, exeName), 0o755);
        exeFound = true;
      } else if (isSharedLib(base)) {
        const data = await entry.getData(new Uint8ArrayWriter());
        await writeBytesTo(data, join(libDir, base), 0o644);
      }
    }
  } finally {
    await reader.close();
  }
  if (!exeFound) {
    throw new AppError("extract_failed", `archive for ${kind} missing ${exeName}`);
  }
}

// Extract ONLY the shared libraries from a companion archive into
// bin/lib/<kind>/, ignoring everything else. Used for runtime-library archives
// that carry no executable (e.g. the Windows CUDA `cudart` bundle:
// cudart64_*.dll, cublas*.dll), so the exe-required check in extractArchive
// would wrongly reject them. Dispatches on extension like extractArchive.
export async function extractLibsOnly(
  archivePath: string,
  targetDir: string,
  kind: BinaryKind,
): Promise<void> {
  const libDir = libDirFor(targetDir, kind);
  await Deno.mkdir(libDir, { recursive: true });
  if (archivePath.toLowerCase().endsWith(".zip")) {
    const bytes = await Deno.readFile(archivePath);
    const reader = new ZipReader(new Uint8ArrayReader(bytes));
    try {
      for (const entry of await reader.getEntries()) {
        if (entry.directory || !entry.getData) continue;
        const base = entry.filename.split("/").pop() ?? entry.filename;
        if (!isSharedLib(base)) continue;
        const data = await entry.getData(new Uint8ArrayWriter());
        await writeBytesTo(data, join(libDir, base), 0o644);
      }
    } finally {
      await reader.close();
    }
    return;
  }
  const file = await Deno.open(archivePath, { read: true });
  const gunzip = new DecompressionStream("gzip");
  const entries = file.readable.pipeThrough(gunzip).pipeThrough(new UntarStream());
  for await (const entry of entries) {
    const path = entry.path.replace(/^\.\//, "");
    const base = path.split("/").pop() ?? path;
    if (entry.header.typeflag !== "0" && entry.header.typeflag !== "") {
      await entry.readable?.cancel();
      continue;
    }
    if (isSharedLib(base)) {
      await writeStreamTo(entry.readable, join(libDir, base), 0o644);
    } else {
      await entry.readable?.cancel();
    }
  }
}

// Matches shared-library filenames: libfoo.so, libfoo.so.1.2, foo.dylib, foo.dll.
function isSharedLib(name: string): boolean {
  return /\.(so|dylib|dll)(\.\d+)*$/i.test(name);
}

async function writeBytesTo(bytes: Uint8Array, outPath: string, mode: number): Promise<void> {
  await Deno.writeFile(outPath, bytes);
  if (Deno.build.os !== "windows") await Deno.chmod(outPath, mode);
}

async function writeStreamTo(
  stream: ReadableStream<Uint8Array> | undefined | null,
  outPath: string,
  mode: number,
): Promise<void> {
  if (!stream) {
    throw new AppError("extract_failed", `null stream for ${outPath}`);
  }
  // Stream to a temp path and rename into place only after the stream fully
  // drains, so a mid-stream pipeTo error never leaves a partial (and, for an
  // exe, launchable) file at the final path. pipeTo closes the file on both
  // success and source-abort.
  const tmpPath = outPath + ".part";
  const out = await Deno.open(tmpPath, {
    create: true,
    write: true,
    truncate: true,
  });
  try {
    await stream.pipeTo(out.writable);
  } catch (err) {
    try {
      await Deno.remove(tmpPath);
    } catch {
      /* fine */
    }
    throw err;
  }
  if (Deno.build.os !== "windows") {
    await Deno.chmod(tmpPath, mode);
  }
  await Deno.rename(tmpPath, outPath);
}

async function fileExists(path: string): Promise<boolean> {
  try {
    const st = await Deno.stat(path);
    return st.isFile;
  } catch {
    return false;
  }
}

/** Best-effort download-size probe: HEAD the URL for its Content-Length.
 *  Returns undefined on any failure (size is advisory, never load-bearing). */
async function headContentLength(url: string): Promise<number | undefined> {
  try {
    const res = await fetch(url, { method: "HEAD" });
    await res.body?.cancel();
    const cl = res.headers.get("content-length");
    if (cl) {
      const n = Number(cl);
      if (Number.isFinite(n)) return n;
    }
  } catch {
    // best-effort
  }
  return undefined;
}
