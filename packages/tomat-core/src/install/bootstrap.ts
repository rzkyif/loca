// One-time secret + content bootstrap for a core install.
//
// Runs before (or as part of) install-service: it ensures the core directory
// tree exists, mints the admin token that authorizes pairing-code minting over
// loopback, optionally seeds the bind-all setting, and plants the built-in
// extension so core installs it OFFLINE on first boot (core re-verifies the
// Ed25519 signature + tarball sha256 before extracting - see extensions/
// seeding.ts). Idempotent: an existing token / settings / planted extension is
// left untouched.

import { join } from "@std/path";
import { BUILTIN_EXTENSION_ID, errMessage } from "@tomat/shared";
import { channel, ensureDirs, paths } from "../paths.ts";
import { builtinExtensionManifestUrl } from "../config.ts";
import { Sha256Stream, toHex } from "../shared/hash.ts";
import { fetchWithTimeout, streamDownload } from "../shared/net.ts";
import { progress } from "./io.ts";
import { run } from "./proc.ts";

export interface BootstrapOptions {
  /** Seed settings.json with server.bindHost=0.0.0.0 so LAN devices can pair
   *  (TOMAT_INSTALL_BIND_ALL=1). */
  bindAll: boolean;
  /** Seed settings.json with server.behindProxy=true so pairing trusts the
   *  HTTPS proxy's real certificate instead of pinning the Core's own
   *  (TOMAT_INSTALL_BEHIND_PROXY=1). Must be set before the first pair. */
  behindProxy: boolean;
}

export async function bootstrap(opts: BootstrapOptions): Promise<void> {
  await ensureDirs();
  await ensureAdminToken();
  await seedServerSettings(opts);
  await plantBuiltinExtension();
}

async function ensureAdminToken(): Promise<void> {
  const file = paths().adminTokenFile;
  if (await fileNonEmpty(file)) {
    progress("admin token already present");
    return;
  }
  const token = toHex(crypto.getRandomValues(new Uint8Array(16)));
  await Deno.writeTextFile(file, token);
  await restrictToOwner(file);
  progress("wrote admin token");
}

// Seed the install-time server settings (bind-all, behind-proxy) into a fresh
// settings.json. Both keys are deliberately not API-writable (a paired client
// must not widen exposure or flip trust), so install time is the one moment
// they can be set without hand-editing the file. An existing settings.json is
// left untouched: a re-run must not clobber a configured core.
async function seedServerSettings(opts: BootstrapOptions): Promise<void> {
  const seed: Record<string, unknown> = {};
  if (opts.bindAll) seed["server.bindHost"] = "0.0.0.0";
  if (opts.behindProxy) seed["server.behindProxy"] = true;
  if (Object.keys(seed).length === 0) return;
  const file = paths().settingsFile;
  if (await fileExists(file)) {
    progress("settings.json already present; leaving server settings as-is");
    return;
  }
  await Deno.writeTextFile(file, JSON.stringify(seed) + "\n");
  progress(`seeded settings.json (${Object.keys(seed).join(", ")})`);
}

/** Turn on server.behindProxy for an ALREADY-installed core by merging the key
 *  into its settings.json (creating the file if absent, keeping every other
 *  key). Unlike the fresh-install seed above, this is an additive
 *  read-modify-write: the client's "install, pair, then flip" flow calls it
 *  AFTER the local loopback pair, because a proxy-served core folds no cert pin
 *  and so cannot be paired over loopback (see the enable-behind-proxy verb in
 *  cli.ts, which restarts the core afterward so the setting takes effect). The
 *  core preserves unknown keys on its own settings writes, so this survives.
 *  Idempotent. */
export async function enableBehindProxy(): Promise<void> {
  const file = paths().settingsFile;
  let settings: Record<string, unknown> = {};
  try {
    const parsed = JSON.parse(await Deno.readTextFile(file));
    if (parsed && typeof parsed === "object") settings = parsed as Record<string, unknown>;
  } catch {
    // Absent or unparseable: start fresh. ensureDirs (run by bootstrap during
    // install, before any pair) already created the parent directory.
  }
  if (settings["server.behindProxy"] === true) {
    progress("server.behindProxy already on");
    return;
  }
  settings["server.behindProxy"] = true;
  // Match the core's own settings writer (JSON.stringify(value, null, 2)) so a
  // later core write produces no spurious reformat diff.
  await Deno.writeTextFile(file, JSON.stringify(settings, null, 2));
  progress("turned on server.behindProxy");
}

// Plant `.tomat-extension-builtin.{tgz,json}` beside the extensions dir. The
// filenames mirror extensions/seeding.ts's planted{Tarball,Manifest}(). Every
// failure is non-fatal: core fetches + verifies + seeds the built-in itself when
// the plant is missing. Skipped on dev (the samples/built-in resolve from the
// codebase there) and when the files are already present (native installer
// payload / re-run).
async function plantBuiltinExtension(): Promise<void> {
  if (channel() === "dev") return;
  const tarballDest = join(paths().extensionsDir, `.${BUILTIN_EXTENSION_ID}.tgz`);
  const manifestDest = join(paths().extensionsDir, `.${BUILTIN_EXTENSION_ID}.json`);
  if ((await fileExists(tarballDest)) && (await fileExists(manifestDest))) {
    progress("built-in extension already planted");
    return;
  }
  try {
    const res = await fetchWithTimeout(builtinExtensionManifestUrl());
    if (!res.ok) {
      progress(`built-in extension manifest unavailable (HTTP ${res.status}); core will seed`);
      return;
    }
    const raw = await res.text();
    const manifest = JSON.parse(raw) as { tarballUrl?: string; sha256?: string };
    if (!manifest.tarballUrl || !manifest.sha256) {
      progress("built-in extension manifest incomplete; core will seed");
      return;
    }
    await Deno.mkdir(paths().stagingDir, { recursive: true });
    const tmp = join(paths().stagingDir, `builtin-extension-${crypto.randomUUID()}.tgz`);
    // sha256 here is only a transport-corruption guard; core re-verifies the
    // manifest signature + tarball hash OFFLINE before it installs on boot.
    const got = await downloadToFile(manifest.tarballUrl, tmp);
    if (got !== manifest.sha256.toLowerCase()) {
      await Deno.remove(tmp).catch(() => {});
      progress("built-in extension checksum mismatch; core will seed");
      return;
    }
    await Deno.rename(tmp, tarballDest);
    // The signed manifest must sit beside the tarball or core can't verify +
    // install offline. Write BOM-free so Deno's JSON.parse accepts it.
    await Deno.writeTextFile(manifestDest, raw);
    progress("planted built-in extension for offline seed");
  } catch (err) {
    progress(`built-in extension plant skipped (core will seed): ${errMessage(err)}`);
  }
}

// --- helpers --------------------------------------------------------------

async function downloadToFile(url: string, outPath: string): Promise<string> {
  const file = await Deno.open(outPath, { create: true, write: true, truncate: true });
  const sha = new Sha256Stream();
  try {
    // The built-in tarball is hashed as-is (not decompressed); the stall guard
    // aborts a dead connection instead of hanging install-service at "Starting
    // the Core".
    await streamDownload(
      url,
      async (chunk) => {
        await file.write(chunk);
        sha.update(chunk);
      },
      { decompress: false },
    );
  } finally {
    file.close();
  }
  return await sha.hexDigest();
}

// Owner-only permissions on the admin token: chmod 0600 on unix, an
// inheritance-stripped owner-full ACL on Windows.
async function restrictToOwner(file: string): Promise<void> {
  if (Deno.build.os === "windows") {
    await run(["icacls", file, "/inheritance:r", "/grant:r", `${username()}:F`], {
      ignoreError: true,
    });
    return;
  }
  await Deno.chmod(file, 0o600);
}

function username(): string {
  return Deno.env.get("USERNAME") ?? Deno.env.get("USER") ?? "";
}

async function fileExists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch {
    return false;
  }
}

async function fileNonEmpty(path: string): Promise<boolean> {
  try {
    const st = await Deno.stat(path);
    return st.isFile && st.size > 0;
  } catch {
    return false;
  }
}
