// Core self-update.
//
// Flow:
//   1. Fetch the core update manifest from CORE_MANIFEST_URL.
//   2. Verify Ed25519 signature against the embedded core public key.
//   3. Pick the entry for the current triple, download to staging/, sha256-verify
//      (with the worker/helper artifacts), then commit the worker/helper renames.
//   4. Swap in the new binary IN PLACE while core still runs (Unix renames over
//      the running inode; Windows renames the running .exe aside, since a running
//      .exe can be renamed just not overwritten), then restart THROUGH the OS
//      supervisor so exactly one owner relaunches core: launchd's KeepAlive and a
//      systemd `--user restart` bring it back on Unix; on Windows the
//      loca-core-updater helper starts the scheduled task (fast path) with a
//      non-zero exit arming Task Scheduler's restart-on-failure as a backstop.
//   5. Write the rollback marker, then exit.
//
// The previous binary is always preserved as `<name>.old` (Unix via a hard link
// before the atomic rename; Windows because the running .exe is renamed there) so
// boot-time rollback can restore it if the new binary crash-loops; the anchor is
// deleted once the update commits at its healthy checkpoint.

import { verifyAsync } from "@noble/ed25519";
import { canonicalize, decodeBase64, errMessage } from "@loca/shared";
import { join } from "@std/path";
import type { CoreManifest, ErrorCode } from "@loca/shared";
import { CORE_VERSION, coreManifestUrl } from "../config.ts";
import { paths } from "../paths.ts";
import { AppError } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { compareSemver } from "../shared/semver.ts";
import { Sha256Stream } from "../shared/hash.ts";
import { fetchWithTimeout, streamDownload } from "../shared/net.ts";
import { binPath, channelBinName } from "../paths.ts";
import { coreBinaryName, hostTriple, platformExe } from "../binaries/versions.ts";
import signingKeys from "../../data/signing-keys.json" with { type: "json" };
import { writeUpdateMarker } from "./rollback.ts";
import { resolveSupervisor } from "../install/service.ts";
import { runPwsh } from "../install/proc.ts";

const log = getLogger("update");

export type UpdateEvent =
  | { kind: "staged"; version: string }
  | { kind: "error"; code: ErrorCode; message: string };

const updateSubscribers = new Set<(e: UpdateEvent) => void>();

export function subscribeUpdate(cb: (e: UpdateEvent) => void): () => void {
  updateSubscribers.add(cb);
  return () => {
    updateSubscribers.delete(cb);
  };
}

export function emitUpdate(e: UpdateEvent): void {
  for (const cb of updateSubscribers) {
    try {
      cb(e);
    } catch (err) {
      log.warn(`update subscriber threw: ${errMessage(err)}`);
    }
  }
}

export function __resetUpdateSubscribersForTesting(): void {
  updateSubscribers.clear();
}

export interface UpdateCheckResult {
  currentVersion: string;
  latestVersion: string;
  available: boolean;
  manifestUrl: string;
}

export async function checkForUpdate(): Promise<UpdateCheckResult> {
  const manifest = await fetchCoreManifest();
  return {
    currentVersion: CORE_VERSION,
    latestVersion: manifest.version,
    // Only a STRICTLY NEWER version is an available update. A published downgrade
    // (or equal version) reports available=false, consistent with applyUpdate,
    // which hard-refuses a downgrade - so the UI never offers an "update" that the
    // apply step would then reject.
    available: compareSemver(manifest.version, CORE_VERSION) > 0,
    manifestUrl: coreManifestUrl(),
  };
}

export async function applyUpdate(targetVersion?: string): Promise<void> {
  try {
    await applyUpdateInner(targetVersion);
  } catch (err) {
    if (err instanceof AppError) {
      emitUpdate({ kind: "error", code: err.code, message: err.message });
    } else {
      const message = errMessage(err);
      emitUpdate({ kind: "error", code: "update_failed", message });
    }
    throw err;
  }
}

async function applyUpdateInner(targetVersion?: string): Promise<void> {
  const manifest = await fetchCoreManifest();
  if (targetVersion && targetVersion !== manifest.version) {
    throw new AppError(
      "update_failed",
      `manifest version ${manifest.version} does not match requested ${targetVersion}`,
    );
  }
  // Refuse downgrades. The manifest is signed (verified upstream of this
  // call) but a compromised signing key OR a roll-back of the published
  // manifest is still possible, and a downgrade could re-introduce a fixed
  // vulnerability.
  if (compareSemver(manifest.version, CORE_VERSION) < 0) {
    throw new AppError(
      "update_failed",
      `refusing downgrade: manifest v${manifest.version} is older than ` +
        `running v${CORE_VERSION}`,
    );
  }
  const triple = hostTriple();
  const entry = manifest.binaries.find((b) => b.triple === triple);
  if (!entry) {
    throw new AppError("update_failed", `no binary for triple ${triple} in manifest`);
  }
  // Clear any artifacts a previously-failed/partial update left behind so
  // staging only ever holds the set we are about to verify (it never
  // accumulates). Safe: applyUpdateInner runs once then exits for the updater.
  await Deno.remove(paths().stagingDir, { recursive: true }).catch(() => {});
  await Deno.mkdir(paths().stagingDir, { recursive: true });
  const stagedPath = join(
    paths().stagingDir,
    `${channelBinName("loca-core")}-${manifest.version}${exeSuffix()}`,
  );
  await downloadAndVerify(entry.url, stagedPath, entry.sha256);

  // Download + verify EVERY worker/helper artifact into staging FIRST, recording
  // where each should land. The renames into the live directories happen only
  // after all have verified (just before the handoff). A mid-update failure
  // (network drop on artifact N) then leaves staging dirty but never half-installs
  // the manifest: the live workers/helpers stay consistent with the old binary
  // until the whole set is ready.
  const pendingRenames: Array<{ from: string; to: string; label: string }> = [];

  // Workers are platform-independent .ts files. A new binary doesn't break old
  // workers and vice versa (the spawn protocol is stable).
  if (manifest.workers && manifest.workers.length > 0) {
    await Deno.mkdir(paths().workersDir, { recursive: true });
    for (const w of manifest.workers) {
      const tmpPath = join(paths().stagingDir, `${w.name}.${manifest.version}`);
      await downloadAndVerify(w.url, tmpPath, w.sha256);
      pendingRenames.push({
        from: tmpPath,
        to: join(paths().workersDir, w.name),
        label: `worker ${w.name}`,
      });
    }
  }

  // Helpers are per-triple native binaries (keychain, updater, hwinfo, ptyhost).
  // Only the entries matching our triple apply. They are committed here, just
  // before the binary swap, to minimize skew. NOTE: this is NOT atomic with the
  // binary swap, and boot rollback (rollback.ts) restores ONLY `<bin>.old`, not
  // the helpers/workers. So a rollback leaves the NEW helpers next to the OLD
  // core. That is tolerated because every helper/worker speaks a stable subprocess
  // protocol (a new one works against an old core and vice versa); if that ever
  // stops holding, this commit must move to the healthy checkpoint (commitUpdate)
  // and grow its own rollback anchors.
  if (manifest.helpers && manifest.helpers.length > 0) {
    const exe = platformExe();
    for (const h of manifest.helpers) {
      if (h.triple !== triple) continue;
      const tmpPath = join(paths().stagingDir, `${h.name}.${manifest.version}${exe}`);
      await downloadAndVerify(h.url, tmpPath, h.sha256);
      pendingRenames.push({
        from: tmpPath,
        to: binPath(`${h.name}${exe}`),
        label: `helper ${h.name}`,
      });
    }
  }

  const currentBin = binPath(coreBinaryName("loca-core"));
  const updaterBin = binPath(coreBinaryName("loca-core-updater"));
  const supervisor = await resolveSupervisor();
  const onWindows = Deno.build.os === "windows";

  // The updater is the out-of-core relauncher. In background mode it is the ONLY
  // thing that can relaunch core, so a missing one is fatal and we bail BEFORE
  // committing any rename (a broken install leaves the live dirs untouched). On
  // Windows it is only the fast path: Task Scheduler's restart-on-failure (armed
  // by the non-zero exit below) is a backstop, so a missing updater just costs
  // restart latency.
  const updaterReady = await pathExists(updaterBin);
  if (supervisor.kind === "background" && !updaterReady) {
    throw new AppError(
      "update_failed",
      `loca-core-updater not installed at ${updaterBin}; ` +
        `rebuild + ship the updater binary or reinstall the Core`,
    );
  }
  if (onWindows && !updaterReady) {
    log.warn(
      `loca-core-updater missing at ${updaterBin}; ` +
        `relying on Task Scheduler restart-on-failure to relaunch core`,
    );
  }

  // Commit the workers/helpers now, as close to the binary swap as possible to
  // minimize the window where they could be skewed against the running (old)
  // binary. commitRename tolerates a running (locked) Windows target by moving
  // it aside first.
  for (const r of pendingRenames) {
    await commitRename(r.from, r.to, onWindows);
    log.info(`updated ${r.label}`);
  }

  // Swap the binary in place and preserve the previous one as `<bin>.old` for
  // boot rollback. This works while core is running on BOTH platforms: Unix
  // renames over the running inode; Windows renames the running .exe aside (a
  // running .exe can be renamed, just not overwritten) then renames the new one
  // in. On the rare Windows failure (AV lock, cross-volume staging) fall back to
  // letting the updater do the swap once core is down.
  let swappedInCore = false;
  try {
    await swapBinaryInPlace(stagedPath, currentBin, onWindows);
    swappedInCore = true;
  } catch (err) {
    // Unix has no updater fallback for the swap, so a failure there is fatal.
    if (!onWindows) throw err;
    log.warn(`in-core binary swap failed (${errMessage(err)}); deferring to the updater`);
  }

  // Write the rollback marker BEFORE the restart. The new binary checks it on
  // first boot and commits once it reaches a healthy checkpoint; a crash before
  // that trips the boot-time rollback to `<bin>.old`.
  await writeUpdateMarker({
    version: manifest.version,
    previousVersion: CORE_VERSION,
  });

  // Restart THROUGH the supervisor so exactly one owner relaunches core and no
  // two relaunchers race over the port. Each path has a fallback:
  //   - launchd: `KeepAlive` relaunches the already-swapped binary the instant
  //     we exit, so there is nothing to spawn (a competing updater-spawned core
  //     is exactly the bug this replaces).
  //   - systemd: a clean exit doesn't trip `Restart=on-failure`, so enqueue a
  //     restart with the user manager and AWAIT it: `--no-block` returns as soon
  //     as the job is queued (owned by the manager, outside this unit's cgroup),
  //     and awaiting guarantees it is queued before we exit and the cgroup is
  //     torn down. If systemctl is unreachable/fails, fall back to exiting
  //     non-zero so the unit's own `Restart=on-failure` relaunches core.
  //   - schtask (Windows): the updater starts the task (fast path) AND we exit
  //     non-zero so Task Scheduler's own restart-on-failure relaunches the
  //     already-swapped binary as a backstop if the updater was torn down with
  //     core's job. The updater only performs the swap when the in-core one
  //     couldn't.
  //   - background: no supervisor, so the updater is the sole relauncher.
  let exitCode = 0;
  switch (supervisor.kind) {
    case "launchd":
      break;
    case "systemd": {
      const rc = await new Deno.Command("systemctl", {
        args: ["--user", "restart", "--no-block", supervisor.unit],
        stdin: "null",
        stdout: "null",
        stderr: "null",
      })
        .output()
        .catch(() => null);
      if (!rc?.success) {
        log.warn(
          `systemctl restart of ${supervisor.unit} failed; ` +
            `falling back to a non-zero exit so Restart=on-failure relaunches core`,
        );
        exitCode = 75;
      }
      break;
    }
    case "schtask":
      if (updaterReady) {
        await spawnUpdater(updaterBin, currentBin, {
          staged: swappedInCore ? undefined : stagedPath,
          startTask: supervisor.task,
        });
      }
      // Non-zero marks this run as failed so Task Scheduler's restart-on-failure
      // (RestartCount/RestartInterval, set at install) relaunches core even if
      // the updater never ran. Any concurrent double-launch is deduped by the
      // task's default MultipleInstances=IgnoreNew policy.
      exitCode = 75;
      break;
    case "background":
      // Unix already swapped in place above; only Windows may hand the swap to
      // the updater. Either way the updater is what respawns core.
      await spawnUpdater(updaterBin, currentBin, {
        staged: swappedInCore ? undefined : stagedPath,
      });
      break;
  }

  emitUpdate({ kind: "staged", version: manifest.version });
  log.info(`update ${manifest.version} staged; restarting core via ${supervisor.kind}`);
  // Give a spawned restarter a beat to detach.
  await new Promise((r) => setTimeout(r, 200));
  Deno.exit(exitCode);
}

async function pathExists(p: string): Promise<boolean> {
  try {
    await Deno.stat(p);
    return true;
  } catch {
    return false;
  }
}

/** Swap the staged binary over the running one and preserve the previous binary
 *  as `<bin>.old` for boot rollback. Works while core is running on both
 *  platforms:
 *   - Unix: hard-link current -> `<bin>.old` (a copy is the fallback on
 *     filesystems without hard links), then rename the staged binary over the
 *     anchored path; the live process keeps its now-unlinked inode.
 *   - Windows: a running .exe can't be overwritten but CAN be renamed, so move
 *     current -> `<bin>.old` (that move IS the anchor) then rename the staged
 *     binary into place; the live process keeps running from the renamed image.
 *     If the second rename fails, restore the running binary so the supervisor
 *     still finds one.
 *  The anchor is mandatory: without it boot rollback could not recover.
 *  `onWindows` is passed explicitly (not read from `Deno.build.os`) so tests can
 *  exercise both branches on any host, mirroring the Rust updater's swap. */
export async function swapBinaryInPlace(
  staged: string,
  current: string,
  onWindows: boolean,
): Promise<void> {
  const old = `${current}.old`;
  await Deno.remove(old).catch(() => {});
  if (onWindows) {
    await Deno.rename(current, old);
    try {
      await Deno.rename(staged, current);
    } catch (err) {
      // Put the running .exe back so the supervisor still has a binary to launch.
      await Deno.rename(old, current).catch(() => {});
      throw err;
    }
    return;
  }
  try {
    await Deno.link(current, old);
  } catch {
    await Deno.copyFile(current, old).catch(() => {});
  }
  try {
    await Deno.stat(old);
  } catch {
    throw new AppError(
      "update_failed",
      `could not create rollback anchor ${old}; refusing to install without a recoverable fallback`,
    );
  }
  await Deno.rename(staged, current);
  await Deno.chmod(current, 0o755);
}

/** Rename a staged artifact into its live path, tolerating a running (locked)
 *  Windows target. A running .exe (e.g. a helper sidecar) can't be overwritten
 *  but can be renamed, so move the live target aside first, then rename the new
 *  one in. The aside copy is best-effort removed on the next update, so it stays
 *  bounded to one per artifact. Unix renames over the target directly (replacing
 *  a running inode is fine). `onWindows` is explicit for the same testability
 *  reason as `swapBinaryInPlace`. */
export async function commitRename(from: string, to: string, onWindows: boolean): Promise<void> {
  try {
    await Deno.rename(from, to);
    return;
  } catch (err) {
    if (!onWindows) throw err;
    const aside = `${to}.old`;
    await Deno.remove(aside).catch(() => {});
    await Deno.rename(to, aside);
    await Deno.rename(from, to);
  }
}

/** Spawn the updater to finish the handoff from a process outside core: on
 *  Windows it starts the Task Scheduler task (`startTask`) and, only if the
 *  in-core swap failed, first installs the staged binary; in Unix background mode
 *  the binary is already swapped, so it only respawns. On Windows it is launched
 *  via `Start-Process` (the same detach path `startBackground` uses); if that
 *  process is still torn down with core's job, the caller's non-zero exit arms
 *  Task Scheduler's restart-on-failure as a backstop. */
async function spawnUpdater(
  updaterBin: string,
  currentBin: string,
  opts: { staged?: string; startTask?: string },
): Promise<void> {
  const args = ["--current", currentBin, "--restart-args", JSON.stringify(Deno.args)];
  if (opts.staged) args.push("--staged", opts.staged);
  if (opts.startTask) args.push("--supervisor", "schtask", "--service-label", opts.startTask);
  if (Deno.build.os === "windows") {
    const argList = args.map((a) => `'${a.replaceAll("'", "''")}'`).join(",");
    await runPwsh(
      `Start-Process -FilePath '${updaterBin.replaceAll("'", "''")}' ` +
        `-WindowStyle Hidden -ArgumentList ${argList}`,
      { capture: false, ignoreError: true },
    );
    return;
  }
  new Deno.Command(updaterBin, {
    args,
    stdin: "null",
    stdout: "null",
    stderr: "null",
  }).spawn();
}

// --- internals ------------------------------------------------------------

// Exported so the fresh-install path (install/fetch-verify.ts) reuses the exact
// same signed-manifest fetch + Ed25519 verify as self-update, keeping one
// audited trust path instead of a parallel copy.
export async function fetchCoreManifest(): Promise<CoreManifest> {
  let res: Response;
  try {
    res = await fetchWithTimeout(coreManifestUrl());
  } catch (err) {
    throw new AppError("manifest_fetch_failed", `core manifest fetch failed: ${errMessage(err)}`);
  }
  if (!res.ok) {
    throw new AppError(
      "manifest_fetch_failed",
      `core manifest HTTP ${res.status} ${res.statusText}`,
    );
  }
  const raw = await res.text();
  let parsed: CoreManifest;
  try {
    parsed = JSON.parse(raw) as CoreManifest;
  } catch (err) {
    throw new AppError("manifest_fetch_failed", `invalid core manifest JSON: ${errMessage(err)}`);
  }
  // Validate the shape before touching any field (mirrors the binary-manifest
  // verifier), so a hostile/garbled response can't reach decodeBase64/verify
  // with an undefined signature and throw unwrapped.
  assertCoreManifestShape(parsed);
  const pk = decodeBase64(signingKeys.publicKey);
  const ok = await verifyCoreManifestSignature(parsed, pk);
  if (!ok) {
    throw new AppError("signature_invalid", "core manifest signature invalid");
  }
  return parsed;
}

/** Verify a core-manifest signature: Ed25519 over signedManifestPayload (=
 *  canonicalize(manifest minus `signature`)), so the signed version + binaries +
 *  workers[] + helpers[] (all downloaded and EXECUTED) are authenticated as one
 *  unit. Key-injectable + pure so it's unit-testable with a throwaway keypair;
 *  the production path passes the embedded signing key. Exported for testing,
 *  mirroring verifyBinariesSignature / verifyBuiltinManifestSignature so all
 *  three trust-root verifiers share one shape and each has accept/reject tests. */
export async function verifyCoreManifestSignature(
  manifest: CoreManifest,
  publicKey: Uint8Array,
): Promise<boolean> {
  const sig = decodeBase64(manifest.signature);
  const body = new TextEncoder().encode(
    signedManifestPayload(manifest as unknown as Record<string, unknown>),
  );
  return await verifyAsync(sig, body, publicKey);
}

/** Validate a fetched core manifest's shape before any field is read or the
 *  signature is verified. Mirrors `assertManifestShape` in binaries/manifest.ts
 *  so the three trust-root fetchers behave identically on a garbled response. */
function assertCoreManifestShape(value: unknown): asserts value is CoreManifest {
  if (!value || typeof value !== "object") {
    throw new AppError("manifest_fetch_failed", "core manifest is not an object");
  }
  const o = value as Record<string, unknown>;
  if (o.schemaVersion !== 1) {
    throw new AppError("manifest_fetch_failed", "bad core manifest schemaVersion");
  }
  if (typeof o.signature !== "string" || o.signature.length === 0) {
    throw new AppError("manifest_fetch_failed", "core manifest missing signature");
  }
  if (typeof o.version !== "string" || o.version.length === 0) {
    throw new AppError("manifest_fetch_failed", "core manifest missing version");
  }
  if (!Array.isArray(o.binaries)) {
    throw new AppError("manifest_fetch_failed", "core manifest binaries is not an array");
  }
  for (const key of ["workers", "helpers"] as const) {
    if (o[key] !== undefined && !Array.isArray(o[key])) {
      throw new AppError("manifest_fetch_failed", `core manifest ${key} is not an array`);
    }
  }
}

// Exported for reuse by the fresh-install path (install/fetch-verify.ts): the
// gzip-decompress + streaming-sha256 + atomic-rename placement is identical for
// a first install and an update.
export async function downloadAndVerify(
  url: string,
  outPath: string,
  sha256: string,
): Promise<void> {
  const tmp = outPath + ".tmp";
  const file = await Deno.open(tmp, {
    create: true,
    write: true,
    truncate: true,
  });
  // Hash incrementally while streaming to disk: the artifact (the ~100MB core
  // binary, model-sized helpers) is never held in memory, only one chunk at a
  // time. Core artifacts ship gzip-compressed and the manifest sha256 is over the
  // DECOMPRESSED file, so streamDownload decompresses before handing us chunks.
  // The stall guard aborts a dead connection instead of hanging the installer.
  const sha = new Sha256Stream();
  try {
    await streamDownload(
      url,
      async (chunk) => {
        await file.write(chunk);
        sha.update(chunk);
      },
      { decompress: true },
    );
  } finally {
    file.close();
  }
  const hex = await sha.hexDigest();
  if (hex !== sha256.toLowerCase()) {
    try {
      await Deno.remove(tmp);
    } catch {
      /* */
    }
    throw new AppError("checksum_mismatch", `update sha256 mismatch: want ${sha256}, got ${hex}`);
  }
  await Deno.rename(tmp, outPath);
  if (Deno.build.os !== "windows") {
    await Deno.chmod(outPath, 0o755);
  }
}

function exeSuffix(): "" | ".exe" {
  return Deno.build.os === "windows" ? ".exe" : "";
}

// canonicalize + decodeBase64 are the signature-critical serializers,
// single-sourced in @loca/shared (imported above for use here) so the signer
// and both verifiers can't drift. Re-exported so self-updater.test.ts can
// import them from this module.
export { canonicalize, decodeBase64 };

/** The exact bytes covered by the core-manifest signature: the whole manifest
 *  minus its `signature` field, canonicalized. The signer
 *  (`scripts/release/core.ts`) MUST sign the identical payload. Keeping this a
 *  single named function makes the coverage testable and prevents anyone
 *  narrowing it back to `{version, binaries}`, which would leave `workers[]`
 *  and `helpers[]` (downloaded and EXECUTED) outside the signature and open a
 *  tampered-manifest code-execution path. */
export function signedManifestPayload(manifest: Record<string, unknown>): string {
  const signed: Record<string, unknown> = { ...manifest };
  delete signed.signature;
  return canonicalize(signed);
}
