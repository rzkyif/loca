// DenoHost secure store: durable secret storage for the vault master key, using
// the OS keychain (via the loca-core-keychain helper) with a chmod-600 file
// fallback under the channel-isolated root. This is the host-specific policy that
// used to live inline in services/secrets.ts; the engine's vault now just asks
// the store to get/set/delete an opaque key and stays runtime-agnostic.
//
// - The dev channel never uses the OS keychain: its helper is an unsigned,
//   rebuilt-at-will binary and macOS ties keychain entries to a code signature,
//   so a key sealed today reads back empty after the next rebuild. Dev keeps the
//   file under the channel-isolated root instead.
// - A keychain write is VERIFIED (read back and compared): an unsigned or
//   oddly-entitled binary can report a successful write that never lands, which
//   would strand the key in a write-only keychain and lose the vault on reboot.
//   Any mismatch counts as "keychain unavailable" so the file fallback is used.
// - get() is non-mutating (no file->keychain migration): callers depend on a
//   read never regenerating or moving the key (see warnIfVaultUnreadable).

import type { HostSecureStore } from "@loca/core-engine";
import { channel, channelKeychainSuffix, paths } from "../paths.ts";
import { errMessage } from "@loca/shared";
import { getLogger } from "../shared/log.ts";
import { keychainDelete, keychainGet, keychainSet } from "../services/keychain.ts";

const log = getLogger("secure-store");

// Stable per-channel keychain service. Stable keeps the bare "com.rzkyif.loca.core" so
// existing entries keep resolving; dev/latest are suffixed.
function keychainService(): string {
  return `com.rzkyif.loca.core${channelKeychainSuffix()}`;
}

function useOsKeychain(): boolean {
  return channel() !== "dev";
}

// File fallback path for one key: a hidden dotfile under the channel root, e.g.
// key "master-key" -> <root>/.master-key (unchanged from the previous layout).
function keyFilePath(key: string): string {
  return `${paths().root}/.${key}`;
}

async function readKeyFile(key: string): Promise<string | null> {
  try {
    return (await Deno.readTextFile(keyFilePath(key))).trim();
  } catch (err) {
    if (err instanceof Deno.errors.NotFound) return null;
    throw new Error(`failed to read ${keyFilePath(key)}: ${errMessage(err)}`);
  }
}

async function writeKeyFile(key: string, value: string): Promise<void> {
  const path = keyFilePath(key);
  // mode at creation closes the world-readable window before the chmod fallback.
  await Deno.writeTextFile(path, value, { mode: 0o600 });
  if (Deno.build.os !== "windows") {
    try {
      await Deno.chmod(path, 0o600);
    } catch {
      /* best-effort */
    }
  }
}

async function sealInKeychainVerified(key: string, value: string): Promise<boolean> {
  const service = keychainService();
  if (!(await keychainSet(service, key, value))) return false;
  const readBack = await keychainGet(service, key);
  if (readBack === value) return true;
  log.warn(
    `OS keychain accepted "${key}" but read-back ` +
      `${readBack === null ? "found no entry" : "returned a different value"}; ` +
      `treating the keychain as unavailable and using the file fallback`,
  );
  return false;
}

export const denoSecureStore: HostSecureStore = {
  // Try the keychain first, then the file. Non-mutating.
  async get(key: string): Promise<string | null> {
    if (useOsKeychain()) {
      const fromKeychain = await keychainGet(keychainService(), key);
      if (fromKeychain) return fromKeychain;
    }
    return await readKeyFile(key);
  },

  // Prefer a verified keychain seal; fall back to the chmod-600 file.
  async set(key: string, value: string): Promise<void> {
    if (useOsKeychain() && (await sealInKeychainVerified(key, value))) {
      log.info(`sealed "${key}" in the OS keychain (service=${keychainService()})`);
      return;
    }
    await writeKeyFile(key, value);
    if (useOsKeychain()) {
      log.warn(
        `stored "${key}" in ${keyFilePath(key)}: OS keychain unavailable ` +
          `(no helper binary, libsecret missing on headless Linux, or a silent ` +
          `dropped write). Back up this file or stored secrets are lost on reinstall.`,
      );
    }
  },

  async delete(key: string): Promise<void> {
    if (useOsKeychain()) await keychainDelete(keychainService(), key);
    try {
      await Deno.remove(keyFilePath(key));
    } catch (err) {
      if (!(err instanceof Deno.errors.NotFound)) throw err;
    }
  },
};
