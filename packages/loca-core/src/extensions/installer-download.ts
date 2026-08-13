// Download phase: acquire a extension's files (fetch/extract an npm tarball, copy
// the built-in, or copy a local folder), validate loca.json, swap the folder
// into place, and register the rows (status 'downloaded') WITHOUT installing deps
// or pinning the content hash. The folder is left byte-identical to what was
// downloaded; we never edit deno.json. Tarballs are integrity-verified before
// extraction and every extracted path is constrained to the extension dir.
import { dirname, join } from "@std/path";
import { UntarStream } from "@std/tar/untar-stream";
import { encodeBase64 } from "@std/encoding/base64";
import { errMessage, seededExtensionById } from "@loca/shared";
import { channel, paths } from "../paths.ts";
import { AppError } from "@loca/core-engine";
import { isWithin } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { sha256Hex, toHex } from "../shared/hash.ts";
import { extensionInstallPath } from "./registry.ts";
import { resolveVersion } from "./npm-registry.ts";
import { loadSeededManifest, seededCodebasePath } from "./seeded-manifest.ts";
import { finishRegister, parseManifestOrThrow } from "./installer-register.ts";
import {
  flattenNpmName,
  type InstallEventSink,
  type InstallSource,
  readOptional,
} from "./installer-shared.ts";

const log = getLogger("extension-installer");

export async function runDownload(
  spec: InstallSource,
  extensionId: string,
  jobId: string,
  sink: InstallEventSink,
): Promise<void> {
  const installPath = extensionInstallPath(extensionId);
  // Defense in depth: whatever the id resolves to must stay inside the extensions
  // dir (covers the flattened-npm-name path too, not just the local slug).
  if (!isWithin(paths().extensionsDir, installPath)) {
    throw new AppError(
      "validation_error",
      `extension "${extensionId}" resolves outside the extensions dir`,
    );
  }
  const stagingPath = installPath + ".new";
  await rmrf(stagingPath);

  try {
    // Extract/copy the bytes AS-IS (no deno.json edit). Deps are installed in
    // the separate install phase; the content hash is pinned there too.
    let version: string;
    if (spec.source === "npm") {
      version = await installNpm(spec, stagingPath, jobId, sink);
    } else if (spec.source === "seeded") {
      version = await installSeeded(spec, stagingPath, jobId, sink);
    } else {
      await installLocal(spec, stagingPath);
      version = "local";
    }

    // Validate loca.json at the folder root (no code execution).
    const manifestText = await readOptional(join(stagingPath, "loca.json"));
    if (!manifestText) {
      throw new AppError("no_loca_json", `no loca.json at root of ${extensionId}`);
    }
    const parsed = parseManifestOrThrow(manifestText);
    const manifestHash = await sha256Hex(manifestText);

    await swapIntoPlace(stagingPath, installPath, extensionId);
    log.info(`downloaded ${extensionId}@${version}`);
    // Register rows; a no-dep extension also finishes at 'installed' here.
    await finishRegister(extensionId, spec.source, version, installPath, parsed, manifestHash);
  } catch (err) {
    await rmrf(stagingPath);
    throw err;
  }
}

// Atomic swap: <id>.new -> <id>.old (if present) + <id>.new -> <id>, with
// rollback of the previous version if the final rename fails.
async function swapIntoPlace(
  stagingPath: string,
  installPath: string,
  extensionId: string,
): Promise<void> {
  await rmrf(installPath + ".old");
  let hadOld = false;
  try {
    await Deno.stat(installPath);
    await Deno.rename(installPath, installPath + ".old");
    hadOld = true;
  } catch {
    /* fresh install */
  }
  try {
    await Deno.rename(stagingPath, installPath);
  } catch (err) {
    if (hadOld) {
      try {
        await Deno.rename(installPath + ".old", installPath);
      } catch (rollbackErr) {
        // The original install dir is now stranded at `installPath.old` and
        // there is no `installPath`. Log so the user can recover manually
        // (the outer error reporting only carries the primary failure).
        log.error(
          `${extensionId}: rollback rename failed; previous version is at ` +
            `${installPath}.old: ${errMessage(rollbackErr)}`,
        );
      }
    }
    throw err;
  }
  await rmrf(installPath + ".old");
}

// --- npm path -------------------------------------------------------------

async function installNpm(
  spec: Extract<InstallSource, { source: "npm" }>,
  stagingPath: string,
  jobId: string,
  sink: InstallEventSink,
): Promise<string> {
  const resolved = await resolveVersion(spec.name, spec.version);
  sink.log(jobId, flattenNpmName(spec.name), "stdout", `resolved ${spec.name}@${resolved.version}`);

  await Deno.mkdir(stagingPath, { recursive: true });
  await fetchAndExtractTarball(resolved.tarballUrl, stagingPath, {
    integrity: resolved.integrity,
    shasum: resolved.shasum,
  });
  // Deps are NOT installed here; that is the separate install phase.
  return resolved.version;
}

// --- seeded path ----------------------------------------------------------

async function installSeeded(
  spec: Extract<InstallSource, { source: "seeded" }>,
  stagingPath: string,
  jobId: string,
  sink: InstallEventSink,
): Promise<string> {
  const ext = seededExtensionById(spec.id);
  if (!ext) {
    throw new AppError("validation_error", `unknown seeded extension "${spec.id}"`);
  }
  // A dev-only extension (samples) is never released or planted in prod, so it
  // must never install outside the dev channel - including via a user-triggered
  // download/update route. The boot seed loop already skips it; this is the
  // chokepoint that also covers the HTTP install paths.
  if (ext.devOnly && channel() !== "dev") {
    throw new AppError(
      "validation_error",
      `${ext.id} is a dev-only extension and cannot be installed on the ${channel()} channel`,
    );
  }
  await Deno.mkdir(stagingPath, { recursive: true });

  if (channel() === "dev") {
    sink.log(jobId, ext.id, "stdout", `installing seeded extension ${ext.id} from codebase`);
    await copyTreeExcludingNodeModules(seededCodebasePath(ext.dir), stagingPath);
    // Dev manifest is codebase-derived (no network).
    return (await loadSeededManifest(ext)).version;
  }

  // First-boot seed (`planted` set): install OFFLINE from the install-script-planted
  // tarball + signed manifest. Seeding already verified the manifest signature;
  // re-verify the tarball against its sha256 (the anchor to that signature) and
  // extract. No network: a running core never fetches on boot - the install-script
  // phase, where network is fine, did the fetching.
  if (spec.planted) {
    const bytes = await readFileOrNull(spec.planted.tarballPath);
    if (!bytes) {
      throw new AppError(
        "tarball_fetch_failed",
        `planted ${ext.id} tarball not found at ${spec.planted.tarballPath}`,
      );
    }
    await verifyTarball(bytes, spec.planted.tarballPath, { sha256: spec.planted.manifest.sha256 });
    sink.log(jobId, ext.id, "stdout", `installing seeded extension ${ext.id} from planted tarball`);
    await extractTarballBytes(bytes, stagingPath);
    return spec.planted.manifest.version;
  }

  // User-triggered download/update (the client invoked a route on a user action):
  // resolving + fetching the signed CDN tarball is allowed here.
  const manifest = await loadSeededManifest(ext);
  sink.log(jobId, ext.id, "stdout", `downloading seeded extension ${ext.id} ${manifest.version}`);
  await fetchAndExtractTarball(manifest.tarballUrl, stagingPath, {
    sha256: manifest.sha256,
  });
  // Deps run in the install phase like every other source; download just gets
  // the files into the folder.
  return manifest.version;
}

// --- local path -----------------------------------------------------------

async function installLocal(
  spec: Extract<InstallSource, { source: "local" }>,
  stagingPath: string,
): Promise<void> {
  // Copy the source tree AS-IS (excluding node_modules). No deno.json edit and
  // no deno install here: deps run in the install phase, and the folder is left
  // byte-identical to what the user placed.
  await Deno.mkdir(stagingPath, { recursive: true });
  await copyTreeExcludingNodeModules(spec.path, stagingPath);
}

// --- helpers --------------------------------------------------------------

async function fetchAndExtractTarball(
  url: string,
  targetDir: string,
  verify?: { integrity?: string; shasum?: string; sha256?: string },
): Promise<void> {
  const res = await fetch(url);
  if (!res.ok) {
    throw new AppError("tarball_fetch_failed", `npm tarball HTTP ${res.status} for ${url}`);
  }
  if (!res.body) {
    throw new AppError("tarball_fetch_failed", `empty tarball body for ${url}`);
  }
  // Verify the tarball BEFORE extraction so a tampered/MITM'd package can't
  // even reach the untar loop. We buffer the (small) tarball, check it against
  // npm's published SRI integrity (sha512) or, failing that, the legacy sha1
  // shasum, then extract from the verified bytes.
  const bytes = new Uint8Array(await res.arrayBuffer());
  await verifyTarball(bytes, url, verify);
  await extractTarballBytes(bytes, targetDir);
}

/** Gunzip + untar already-verified tarball bytes into targetDir, applying the
 *  same zip-slip / symlink / hardlink guards as the fetch path. Callers MUST
 *  verify the bytes (sha256/integrity) before calling: this trusts them. */
async function extractTarballBytes(
  bytes: Uint8Array<ArrayBuffer>,
  targetDir: string,
): Promise<void> {
  const gunzip = new DecompressionStream("gzip");
  const entries = new Blob([bytes]).stream().pipeThrough(gunzip).pipeThrough(new UntarStream());

  for await (const entry of entries) {
    const name = entry.path;
    // npm tarballs always begin with `package/`. Strip it.
    const stripped = name.startsWith("package/") ? name.slice("package/".length) : name;
    if (!stripped) {
      await entry.readable?.cancel();
      continue;
    }
    // Only regular files and directories are extracted. A malicious tarball
    // could otherwise ship a symlink/hardlink (typeflag "2"/"1") or device
    // node and use it to escape the staging dir or follow into a sensitive
    // path on a later write. ustar regular-file typeflags are "0" / "\0" / "".
    const kind = extractableEntryType(entry.header.typeflag);
    if (kind === null) {
      await entry.readable?.cancel();
      throw new AppError(
        "extract_failed",
        `tarball entry "${name}" has unsupported type ${JSON.stringify(
          entry.header.typeflag,
        )} (symlinks/hardlinks/devices are not allowed)`,
      );
    }
    const isDir = kind === "dir";
    const out = join(targetDir, stripped);
    // Zip-slip guard: the resolved destination MUST stay inside targetDir.
    // Without this, an entry like `package/../../../bin/loca-core` would
    // overwrite arbitrary files the core can write (binaries, worker scripts,
    // the secrets vault).
    if (!isWithin(targetDir, out)) {
      await entry.readable?.cancel();
      throw new AppError(
        "extract_failed",
        `tarball entry "${name}" escapes the extension directory`,
      );
    }
    if (isDir) {
      await Deno.mkdir(out, { recursive: true });
      await entry.readable?.cancel();
      continue;
    }
    await Deno.mkdir(dirname(out), { recursive: true });
    const stream = entry.readable;
    if (!stream) continue;
    const file = await Deno.open(out, {
      create: true,
      write: true,
      truncate: true,
    });
    await stream.pipeTo(file.writable);
  }
}

/** ustar typeflag classification for tarball extraction. Regular files are
 *  "0"/"\0"/""; directories are "5". Anything else (symlink "2", hardlink "1",
 *  device/fifo) could escape the staging dir on a later write and is rejected.
 *  Exported for testing. */
export function extractableEntryType(typeflag: string): "file" | "dir" | null {
  if (typeflag === "5") return "dir";
  if (typeflag === "0" || typeflag === "\0" || typeflag === "") return "file";
  return null;
}

/** Verify a fetched npm tarball against npm's published integrity before it is
 *  extracted. Prefers the SRI `dist.integrity` (sha512); falls back to the
 *  legacy `dist.shasum` (sha1). When neither is available the install fails
 *  closed: a supply-chain integrity check that silently downgrades to "no
 *  verification" when the (untrusted) registry metadata omits both fields would
 *  let a poisoned mirror serve arbitrary bytes. npm always ships at least a
 *  shasum, so this never blocks a legitimate install. Exported for testing. */
export async function verifyTarball(
  bytes: Uint8Array,
  url: string,
  verify?: { integrity?: string; shasum?: string; sha256?: string },
): Promise<void> {
  // Plain sha256-hex (used by the built-in extension's signed manifest, which
  // pins the tarball hash directly rather than npm's SRI/shasum metadata).
  if (verify?.sha256) {
    const digest = await crypto.subtle.digest("SHA-256", bytes.buffer as ArrayBuffer);
    const got = toHex(new Uint8Array(digest));
    if (got !== verify.sha256.toLowerCase()) {
      throw new AppError("checksum_mismatch", `tarball failed sha256 check for ${url}`);
    }
    return;
  }
  const sha512Entry = verify?.integrity?.split(/\s+/).find((s) => s.startsWith("sha512-"));
  if (sha512Entry) {
    const expected = sha512Entry.slice("sha512-".length);
    const digest = await crypto.subtle.digest("SHA-512", bytes.buffer as ArrayBuffer);
    const got = encodeBase64(new Uint8Array(digest));
    if (got !== expected) {
      throw new AppError(
        "checksum_mismatch",
        `npm tarball failed sha512 integrity check for ${url}`,
      );
    }
    return;
  }
  if (verify?.shasum) {
    const digest = await crypto.subtle.digest("SHA-1", bytes.buffer as ArrayBuffer);
    const got = toHex(new Uint8Array(digest));
    if (got !== verify.shasum.toLowerCase()) {
      throw new AppError("checksum_mismatch", `npm tarball failed sha1 checksum for ${url}`);
    }
    return;
  }
  throw new AppError(
    "checksum_mismatch",
    `npm tarball ${url} has no integrity/shasum metadata; refusing to install unverified`,
  );
}

async function copyTreeExcludingNodeModules(src: string, dst: string): Promise<void> {
  for await (const entry of Deno.readDir(src)) {
    if (entry.name === "node_modules") continue;
    const s = join(src, entry.name);
    const d = join(dst, entry.name);
    if (entry.isDirectory) {
      await Deno.mkdir(d, { recursive: true });
      await copyTreeExcludingNodeModules(s, d);
    } else if (entry.isFile) {
      await Deno.copyFile(s, d);
    }
  }
}

async function readFileOrNull(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
  try {
    return await Deno.readFile(path);
  } catch {
    return null;
  }
}

async function rmrf(path: string): Promise<void> {
  try {
    await Deno.remove(path, { recursive: true });
  } catch (err) {
    if (err instanceof Deno.errors.NotFound) return;
    throw err;
  }
}
