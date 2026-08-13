// Fresh-install fetch of the core binary set from the signed manifest.
//
// Reuses the SAME fetchCoreManifest (Ed25519 verify) + downloadAndVerify
// (gzip-decompress + streaming sha256 + atomic placement) as self-update, so
// there is one audited trust path for both a first install and an update. The
// only difference from self-update: no rollback marker, no updater handoff, and
// the currently-running core binary is left in place (a process cannot rewrite
// its own executable on Windows, and it is the very binary running this).

import { join } from "@std/path";
import { downloadAndVerify, fetchCoreManifest } from "../update/self-updater.ts";
import { binPath, paths } from "../paths.ts";
import { coreBinaryName, hostTriple, platformExe } from "../binaries/versions.ts";
import { progress } from "./io.ts";

/** Download + verify + place the workers and per-triple helpers named in the
 *  signed core manifest, plus the core binary itself if it is not already on
 *  disk (the caller's seed step normally places it, and this process is running
 *  it). */
export async function selfInstall(): Promise<void> {
  const manifest = await fetchCoreManifest();
  const triple = hostTriple();
  const entry = manifest.binaries.find((b) => b.triple === triple);
  if (!entry) throw new Error(`no core binary for triple ${triple} in manifest`);

  await Deno.mkdir(paths().binDir, { recursive: true });
  await Deno.mkdir(paths().workersDir, { recursive: true });

  const coreBin = binPath(coreBinaryName("loca-core"));
  if (await fileExists(coreBin)) {
    progress("core binary already present; leaving it in place");
  } else {
    await downloadAndVerify(entry.url, coreBin, entry.sha256);
    progress(`installed core binary v${manifest.version}`);
  }

  for (const w of manifest.workers ?? []) {
    await downloadAndVerify(w.url, join(paths().workersDir, w.name), w.sha256);
  }
  if (manifest.workers?.length) progress(`installed ${manifest.workers.length} worker(s)`);

  const exe = platformExe();
  let helperCount = 0;
  for (const h of manifest.helpers ?? []) {
    if (h.triple !== triple) continue;
    await downloadAndVerify(h.url, binPath(`${h.name}${exe}`), h.sha256);
    helperCount++;
  }
  if (helperCount) progress(`installed ${helperCount} helper(s)`);
}

async function fileExists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch {
    return false;
  }
}
