// handleUpdateMarkerOnBoot: decision branches around the update
// marker file. Driven against tempdir-isolated paths().updateMarkerFile.
// The update commits at a healthy checkpoint (commitUpdate), not on a timer,
// so an ordinary restart of a working binary can never trip a rollback.

import { assertEquals } from "@std/assert";
import { CORE_VERSION } from "../config.ts";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import {
  commitUpdate,
  handleUpdateMarkerOnBoot,
  type UpdateMarker,
  writeUpdateMarker,
} from "./rollback.ts";
import { paths } from "../paths.ts";

async function readRaw(): Promise<UpdateMarker | null> {
  try {
    return JSON.parse(await Deno.readTextFile(paths().updateMarkerFile));
  } catch {
    return null;
  }
}

Deno.test("handleUpdateMarkerOnBoot: returns false when no marker exists", async () => {
  const env = await setupTestEnv();
  try {
    assertEquals(await handleUpdateMarkerOnBoot(), false);
  } finally {
    await env.teardown();
  }
});

Deno.test("handleUpdateMarkerOnBoot: clears stale marker for a wholly different version", async () => {
  const env = await setupTestEnv();
  try {
    await writeUpdateMarker({
      version: "999.0.0",
      previousVersion: "998.0.0",
    });
    assertEquals(await handleUpdateMarkerOnBoot(), false);
    assertEquals(await readRaw(), null);
  } finally {
    await env.teardown();
  }
});

Deno.test("handleUpdateMarkerOnBoot: clears marker when running the rolled-back version", async () => {
  const env = await setupTestEnv();
  try {
    // Marker says v999 was being staged FROM CORE_VERSION; we're now back on
    // CORE_VERSION (already rolled back). Branch: previousVersion === running,
    // clear and continue.
    await writeUpdateMarker({
      version: "999.0.0",
      previousVersion: CORE_VERSION,
    });
    assertEquals(await handleUpdateMarkerOnBoot(), false);
    assertEquals(await readRaw(), null);
  } finally {
    await env.teardown();
  }
});

Deno.test("handleUpdateMarkerOnBoot: first boot bumps attempts to 1 and continues", async () => {
  const env = await setupTestEnv();
  try {
    await writeUpdateMarker({
      version: CORE_VERSION,
      previousVersion: "0.0.1",
    });
    assertEquals(await handleUpdateMarkerOnBoot(), false);
    const after = await readRaw();
    assertEquals(after?.attempts, 1);
    assertEquals(after?.version, CORE_VERSION);
  } finally {
    await env.teardown();
  }
});

Deno.test("commitUpdate: a first boot that commits clears the marker + anchor (no rollback on restart)", async () => {
  const env = await setupTestEnv();
  try {
    const currentBin = binPath(coreBinaryName("loca-core"));
    const oldBin = currentBin + ".old";
    await Deno.writeTextFile(currentBin, "WORKING_NEW");
    await Deno.writeTextFile(oldBin, "PREVIOUS");
    await writeUpdateMarker({
      version: CORE_VERSION,
      previousVersion: "0.0.1",
    });

    // First boot: records the attempt, keeps running.
    assertEquals(await handleUpdateMarkerOnBoot(), false);
    assertEquals((await readRaw())?.attempts, 1);

    // Healthy checkpoint: commit. Marker + anchor are gone.
    await commitUpdate();
    assertEquals(await readRaw(), null);
    let anchorThere = true;
    try {
      await Deno.stat(oldBin);
    } catch {
      anchorThere = false;
    }
    assertEquals(anchorThere, false);

    // A later ordinary restart sees no marker and must NOT roll back.
    assertEquals(await handleUpdateMarkerOnBoot(), false);
    assertEquals(await Deno.readTextFile(currentBin), "WORKING_NEW");
  } finally {
    await env.teardown();
  }
});

Deno.test("writeUpdateMarker: persists attempts=0", async () => {
  const env = await setupTestEnv();
  try {
    await writeUpdateMarker({
      version: CORE_VERSION,
      previousVersion: "0.0.1",
    });
    const m = await readRaw();
    assertEquals(m?.attempts, 0);
    assertEquals(m?.version, CORE_VERSION);
    assertEquals(m?.previousVersion, "0.0.1");
  } finally {
    await env.teardown();
  }
});

// --- performRollback (via the attempts>=MAX_BOOT_ATTEMPTS branch) ----------

import { binPath, paths as _paths } from "../paths.ts";
import { coreBinaryName } from "../binaries/versions.ts";

async function writeMarkerWithAttempts(attempts: number): Promise<void> {
  await Deno.writeTextFile(
    _paths().updateMarkerFile,
    JSON.stringify({
      version: CORE_VERSION,
      previousVersion: "0.0.1",
      attempts,
    }),
  );
}

Deno.test("handleUpdateMarkerOnBoot: a single interrupted boot is retried, not rolled back", async () => {
  const env = await setupTestEnv();
  try {
    const currentBin = binPath(coreBinaryName("loca-core"));
    const oldBin = currentBin + ".old";
    await Deno.writeTextFile(currentBin, "NEW_HEALTHY");
    await Deno.writeTextFile(oldBin, "PREVIOUS");
    // attempts=1 means one prior boot that didn't reach the checkpoint (e.g. an
    // external restart during startup). It must NOT roll back yet: bump + retry.
    await writeMarkerWithAttempts(1);
    assertEquals(await handleUpdateMarkerOnBoot(), false);
    assertEquals((await readRaw())?.attempts, 2);
    // The new binary is untouched (not downgraded).
    assertEquals(await Deno.readTextFile(currentBin), "NEW_HEALTHY");
  } finally {
    await env.teardown();
  }
});

Deno.test("handleUpdateMarkerOnBoot: returns false when rollback anchor is missing", async () => {
  const env = await setupTestEnv();
  try {
    await writeMarkerWithAttempts(2);
    // No <bin>.old anchor written, so performRollback should log + bail.
    assertEquals(await handleUpdateMarkerOnBoot(), false);
    // Marker cleared so we don't loop on next boot.
    assertEquals(await readRaw(), null);
  } finally {
    await env.teardown();
  }
});

Deno.test("handleUpdateMarkerOnBoot: rolls back current binary to <bin>.old contents", async () => {
  const env = await setupTestEnv();
  try {
    const currentBin = binPath(coreBinaryName("loca-core"));
    const oldBin = currentBin + ".old";
    await Deno.writeTextFile(currentBin, "BROKEN_V999");
    await Deno.writeTextFile(oldBin, "WORKING_V998");
    await writeMarkerWithAttempts(2);

    assertEquals(await handleUpdateMarkerOnBoot(), true);

    // currentBin now holds previous-version bytes.
    assertEquals(await Deno.readTextFile(currentBin), "WORKING_V998");
    // Broken binary preserved for post-mortem.
    assertEquals(await Deno.readTextFile(currentBin + ".broken"), "BROKEN_V999");
    // Original oldBin is consumed once the copy-first install committed.
    let oldStillThere = true;
    try {
      await Deno.stat(oldBin);
    } catch {
      oldStillThere = false;
    }
    assertEquals(oldStillThere, false);
    // Marker cleared.
    assertEquals(await readRaw(), null);
  } finally {
    await env.teardown();
  }
});

Deno.test("handleUpdateMarkerOnBoot: copy-first failure falls back to two-rename swap (still rolls back)", async () => {
  const env = await setupTestEnv();
  const originalCopyFile = Deno.copyFile;
  try {
    const currentBin = binPath(coreBinaryName("loca-core"));
    const oldBin = currentBin + ".old";
    await Deno.writeTextFile(currentBin, "BROKEN");
    await Deno.writeTextFile(oldBin, "WORKING");
    await writeMarkerWithAttempts(2);

    // Force the copy-first step to fail so the impl falls back to the
    // two-rename pattern (which consumes oldBin directly rather than via
    // the staged copy).
    (Deno as { copyFile: typeof Deno.copyFile }).copyFile = () =>
      Promise.reject(new Error("synthetic copy failure"));

    assertEquals(await handleUpdateMarkerOnBoot(), true);

    // Rollback still completed.
    assertEquals(await Deno.readTextFile(currentBin), "WORKING");
    assertEquals(await Deno.readTextFile(currentBin + ".broken"), "BROKEN");
    // In the fallback path oldBin was renamed (not copied), so it's gone
    // for a different reason than the happy path, but observably absent.
    let oldStillThere = true;
    try {
      await Deno.stat(oldBin);
    } catch {
      oldStillThere = false;
    }
    assertEquals(oldStillThere, false);
  } finally {
    (Deno as { copyFile: typeof Deno.copyFile }).copyFile = originalCopyFile;
    await env.teardown();
  }
});

Deno.test("handleUpdateMarkerOnBoot: a leftover <bin>.broken from a prior attempt is overwritten, not appended", async () => {
  const env = await setupTestEnv();
  try {
    const currentBin = binPath(coreBinaryName("loca-core"));
    const oldBin = currentBin + ".old";
    const brokenBin = currentBin + ".broken";
    await Deno.writeTextFile(currentBin, "NEW_BROKEN");
    await Deno.writeTextFile(oldBin, "WORKING");
    await Deno.writeTextFile(brokenBin, "PRIOR_BROKEN_LEFTOVER");
    await writeMarkerWithAttempts(2);

    assertEquals(await handleUpdateMarkerOnBoot(), true);
    assertEquals(await Deno.readTextFile(brokenBin), "NEW_BROKEN");
    assertEquals(await Deno.readTextFile(currentBin), "WORKING");
  } finally {
    await env.teardown();
  }
});

Deno.test("handleUpdateMarkerOnBoot: latest rolls back the channel-suffixed binary (loca-core-latest)", async () => {
  const priorChannel = Deno.env.get("LOCA_CHANNEL");
  Deno.env.set("LOCA_CHANNEL", "latest");
  const env = await setupTestEnv();
  try {
    // The rollback anchor must follow the channel suffix, not the bare name.
    const currentBin = binPath(coreBinaryName("loca-core"));
    assertEquals(currentBin.includes("loca-core-latest"), true);
    const oldBin = currentBin + ".old";
    await Deno.writeTextFile(currentBin, "BROKEN_LATEST");
    await Deno.writeTextFile(oldBin, "WORKING_LATEST");
    await writeMarkerWithAttempts(2);

    assertEquals(await handleUpdateMarkerOnBoot(), true);
    assertEquals(await Deno.readTextFile(currentBin), "WORKING_LATEST");
  } finally {
    await env.teardown();
    if (priorChannel === undefined) Deno.env.delete("LOCA_CHANNEL");
    else Deno.env.set("LOCA_CHANNEL", priorChannel);
  }
});
