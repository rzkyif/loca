// Self-update rollback on boot.
//
// The marker file (paths().updateMarkerFile) is written by self-updater
// before it hands off to loca-core-updater. When the new core starts:
//
//   - First boot after update: marker exists with attempts=0. We bump it to
//     attempts=1 and continue booting. Once the new binary reaches a healthy
//     checkpoint (its HTTP listener is bound, which implies the DB opened and
//     the TLS key unsealed), main.ts calls commitUpdate(): the marker and the
//     `<bin>.old` rollback anchor are removed and the update is committed.
//     Committing at the healthy checkpoint (rather than after a fixed uptime)
//     means an ordinary restart of a working binary can NEVER trip a rollback,
//     because by the time it restarts the marker is already gone.
//
//   - Marker still present after MAX_BOOT_ATTEMPTS boots: every boot bumped the
//     attempt count without reaching the healthy checkpoint, so the new binary
//     could not run. We roll back by swapping `<bin>.old` back over `<bin>`. The
//     current process exits; the OS supervisor (launchd / systemd-user /
//     Scheduled Task) re-launches `<bin>`, now the previous working version.
//     The threshold is >1 so a single boot INTERRUPTED before the checkpoint (an
//     external restart / reboot during startup, not a crash) is retried rather
//     than mistaken for a failed binary and silently downgraded.
//
//   - Marker present but version unrelated to current: confused state;
//     log + delete marker, continue.

import { coreBinaryName } from "../binaries/versions.ts";
import { errMessage } from "@loca/shared";
import { binPath, paths } from "../paths.ts";
import { getLogger } from "../shared/log.ts";
import { CORE_VERSION } from "../config.ts";

const log = getLogger("update.rollback");

// How many boots the new binary gets to reach its healthy checkpoint before we
// roll back. >1 so a single boot interrupted by an external restart/reboot during
// startup (not a crash) is retried instead of triggering a spurious downgrade.
const MAX_BOOT_ATTEMPTS = 2;

export interface UpdateMarker {
  // Version the updater intended to install (== CORE_VERSION of the new
  // binary on a healthy first boot).
  version: string;
  // Version we replaced (== CORE_VERSION of the `<bin>.old` rollback anchor).
  previousVersion: string;
  // Per-boot attempt count; bumped each time we see the marker before the new
  // binary commits the update at its healthy checkpoint.
  attempts: number;
}

export async function writeUpdateMarker(m: Omit<UpdateMarker, "attempts">): Promise<void> {
  const full: UpdateMarker = { ...m, attempts: 0 };
  const tmp = paths().updateMarkerFile + ".tmp";
  await Deno.writeTextFile(tmp, JSON.stringify(full));
  await Deno.rename(tmp, paths().updateMarkerFile);
}

async function readMarker(): Promise<UpdateMarker | null> {
  try {
    const text = await Deno.readTextFile(paths().updateMarkerFile);
    return JSON.parse(text) as UpdateMarker;
  } catch {
    return null;
  }
}

async function deleteMarker(): Promise<void> {
  try {
    await Deno.remove(paths().updateMarkerFile);
  } catch {
    /* fine */
  }
}

async function bumpAttempts(m: UpdateMarker): Promise<void> {
  const updated: UpdateMarker = { ...m, attempts: m.attempts + 1 };
  const tmp = paths().updateMarkerFile + ".tmp";
  await Deno.writeTextFile(tmp, JSON.stringify(updated));
  await Deno.rename(tmp, paths().updateMarkerFile);
}

function oldBinaryPath(): string {
  // Use the platform's actual binary name + .old suffix. On Windows the
  // updater used `.exe.old`; on Unix it's `<name>.old`. Channel-suffixed so
  // latest rolls back loca-core-latest, not loca-core.
  return binPath(coreBinaryName("loca-core")) + ".old";
}

async function exists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch {
    return false;
  }
}

// Called from main.ts before any service boots. Returns true if rollback
// happened (caller should exit immediately so the supervisor re-launches
// the now-restored old binary).
export async function handleUpdateMarkerOnBoot(): Promise<boolean> {
  const marker = await readMarker();
  if (!marker) return false;

  if (marker.version !== CORE_VERSION) {
    if (marker.previousVersion === CORE_VERSION) {
      // We rolled back already (or the swap never happened); clean up.
      log.info(
        `update marker present but we're already on previousVersion ` + `${CORE_VERSION}; clearing`,
      );
    } else {
      log.warn(
        `update marker version ${marker.version} doesn't match running ` +
          `${CORE_VERSION} and isn't previousVersion ${marker.previousVersion}; ` +
          `clearing`,
      );
    }
    await deleteMarker();
    return false;
  }

  if (marker.attempts >= MAX_BOOT_ATTEMPTS) {
    // Every one of MAX_BOOT_ATTEMPTS boots saw the marker without reaching the
    // healthy checkpoint to commit → the new binary genuinely could not run
    // (not just one interrupted startup). Roll back.
    return await performRollback(marker);
  }

  // Boot after update, under the retry budget. Record the attempt and continue
  // booting; the update commits once main.ts reaches its healthy checkpoint
  // (commitUpdate), and an interrupted boot just retries on the next one.
  await bumpAttempts(marker);
  log.info(
    `update marker: boot ${marker.attempts + 1}/${MAX_BOOT_ATTEMPTS} of v${marker.version} ` +
      `(was v${marker.previousVersion}); commits once it boots healthy`,
  );
  return false;
}

/** Commit a pending self-update: remove the `<bin>.old` rollback anchor and the
 *  marker so a later restart can't roll back a binary that has already reached a
 *  healthy checkpoint. main.ts calls this once the HTTP listener is bound.
 *  Idempotent and a no-op when no update is pending. */
export async function commitUpdate(): Promise<void> {
  if (!(await exists(paths().updateMarkerFile))) return;
  const oldBin = oldBinaryPath();
  try {
    await Deno.remove(oldBin);
    log.info(`update committed; removed rollback anchor ${oldBin}`);
  } catch {
    /* may not exist on first install (fine) */
  }
  await deleteMarker();
}

async function performRollback(marker: UpdateMarker): Promise<boolean> {
  const oldBin = oldBinaryPath();
  if (!(await exists(oldBin))) {
    log.error(
      `update marker says rollback needed but ${oldBin} is missing; ` +
        `cannot recover. Manual reinstall required.`,
    );
    await deleteMarker();
    return false;
  }
  const currentBin = binPath(coreBinaryName("loca-core"));
  const brokenBin = currentBin + ".broken";
  const stagedOld = oldBin + ".staged";

  log.warn(
    `rolling back v${marker.version} → v${marker.previousVersion}: ` +
      `new binary failed to reach a healthy checkpoint`,
  );

  try {
    await Deno.remove(brokenBin);
  } catch {
    /* fine */
  }
  try {
    await Deno.remove(stagedOld);
  } catch {
    /* fine */
  }

  // Best-effort free-space precheck. Deno doesn't ship a statfs/statvfs
  // wrapper, so we shell out to `df` on Unix-likes. Windows skips the
  // precheck (the copy step's failure path is the fallback anyway).
  // Goal: log a clear message before the swap if we're already known to
  // be disk-bound, so the caller doesn't have to infer it from the
  // generic "copy-first staging failed" message that follows.
  try {
    const needed = (await Deno.stat(oldBin)).size;
    const free = await estimateFreeBytes(binPath(""));
    if (free !== null && free < needed * 2) {
      log.warn(
        `low free space (need ~${needed} bytes for safe swap, ` +
          `~${free} bytes free in bin dir); will fall back if copy fails`,
      );
    }
  } catch {
    /* precheck is informational only */
  }

  // Two-phase swap with copy-first staging so the original `oldBin`
  // anchor survives until the install rename has succeeded:
  //   1. copy(oldBin → stagedOld)
  //   2. rename(currentBin → brokenBin)   ← aside (atomic on same FS)
  //   3. rename(stagedOld → currentBin)   ← install (atomic on same FS)
  //   4. remove(oldBin)                   ← committed; original is fungible
  //
  // If step 3 fails, revert step 2 so the supervisor finds *some* binary
  // at currentBin (still the broken one, but better than missing). If the
  // copy in step 1 fails (disk full, permission), fall back to the older
  // two-rename pattern that loses currentBin on a failed install rename.
  let usedCopy = true;
  try {
    await Deno.copyFile(oldBin, stagedOld);
  } catch (err) {
    usedCopy = false;
    log.warn(`copy-first staging failed (${errMessage(err)}); falling back to two-rename swap`);
  }

  try {
    await Deno.rename(currentBin, brokenBin);
  } catch (err) {
    log.error(`rollback aside failed: ${errMessage(err)}`);
    if (usedCopy) {
      try {
        await Deno.remove(stagedOld);
      } catch {
        /* ignore */
      }
    }
    await deleteMarker();
    return false;
  }

  const installSrc = usedCopy ? stagedOld : oldBin;
  try {
    await Deno.rename(installSrc, currentBin);
  } catch (err) {
    log.error(
      `rollback install failed: ${errMessage(err)}; ` +
        `attempting to restore broken binary so supervisor has something to run`,
    );
    try {
      await Deno.rename(brokenBin, currentBin);
    } catch (revertErr) {
      log.error(
        `revert of aside also failed: ${errMessage(revertErr)}. Manual reinstall required.`,
      );
    }
    if (usedCopy) {
      try {
        await Deno.remove(stagedOld);
      } catch {
        /* ignore */
      }
    }
    await deleteMarker();
    return false;
  }

  if (Deno.build.os !== "windows") {
    try {
      await Deno.chmod(currentBin, 0o755);
    } catch {
      /* ignore */
    }
  }

  if (usedCopy) {
    // Committed: the original anchor is no longer needed.
    try {
      await Deno.remove(oldBin);
    } catch {
      /* ignore; fine if anti-virus is holding it */
    }
  }

  await deleteMarker();
  log.info(`rolled back. exiting so the supervisor relaunches v${marker.previousVersion}.`);
  return true;
}

/** Returns approximate free bytes on the filesystem hosting `path`.
 *  Best-effort: shells out to `df` on macOS/Linux, returns null on
 *  Windows or any failure. The caller MUST tolerate `null` (precheck
 *  is informational; the swap path has its own failure fallback). */
async function estimateFreeBytes(path: string): Promise<number | null> {
  if (Deno.build.os === "windows") return null;
  try {
    const cmd = new Deno.Command("df", {
      args: ["-Pk", path],
      stdout: "piped",
      stderr: "null",
    });
    const out = await cmd.output();
    if (!out.success) return null;
    const text = new TextDecoder().decode(out.stdout);
    // `df -Pk` output:
    //   Filesystem  1024-blocks  Used  Available Capacity  Mounted on
    //   /dev/disk1s1  ...  ...  123456  ...  /
    const lines = text.trim().split("\n");
    if (lines.length < 2) return null;
    const cols = lines[1].split(/\s+/);
    if (cols.length < 4) return null;
    const availKb = parseInt(cols[3], 10);
    if (!Number.isFinite(availKb)) return null;
    return availKb * 1024;
  } catch {
    return null;
  }
}
