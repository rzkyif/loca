// Secrets vault for external API keys etc.
//
// The bag of secrets is a single JSON object (keys are free-form names like
// "openai-api-key", values are strings), AES-GCM-256 encrypted with a 32-byte
// master key and persisted as secrets.enc (stored bytes are a 12-byte nonce
// followed by the ciphertext). The whole file is re-encrypted on every write;
// secrets bags are tiny so per-key crypto isn't worth it.
//
// Runtime-agnostic: the master key is durably stored by the host's secure store
// (host().secureStore get/set the "master-key" entry), and secrets.enc is
// read/written through host().fs. The host owns the OS-specific policy (OS
// keychain vs a file fallback, channel rules); see the DenoHost secure store.

import { errMessage } from "@loca/shared";
import { host } from "../platform/runtime.ts";
import { enginePaths } from "../platform/paths.ts";
import { AppError } from "../platform/errors.ts";
import { getLogger } from "../platform/log.ts";

const log = getLogger("secrets");

const NONCE_LEN = 12;
const KEY_LEN = 32;
// The secure-store entry name for the vault master key.
const MASTER_KEY_ID = "master-key";

let cachedKey: CryptoKey | null = null;

type SecretsListener = (names: string[]) => void;
const secretsListeners = new Set<SecretsListener>();

/** Subscribe to vault content changes. Fires with the sorted secret-name list
 *  after every successful set / delete / clear (names only, never values), so
 *  the ws hub can tell clients which secrets are configured. Same pattern as
 *  subscribeCoreSettings: sync registration, fire-and-forget listeners. */
export function subscribeSecretsChanged(fn: SecretsListener): () => void {
  secretsListeners.add(fn);
  return () => secretsListeners.delete(fn);
}

function notifySecretsChanged(names: string[]): void {
  for (const fn of secretsListeners) {
    try {
      fn(names);
    } catch (err) {
      log.warn(`secrets listener failed: ${errMessage(err)}`);
    }
  }
}

// Test-only: drops the cached master key so the next call rebuilds it
// from disk / keychain, and detaches every change listener. Use between
// tests that swap LOCA_CORE_HOME.
export function __resetForTesting(): void {
  cachedKey = null;
  secretsListeners.clear();
}

// Read the stored master key, or null if none exists yet. Non-mutating: never
// generates or moves a key, so warnIfVaultUnreadable can probe safely. Validates
// the length so a corrupt entry doesn't silently produce a wrong key.
async function readMasterKey(): Promise<Uint8Array | null> {
  const encoded = await host().secureStore.get(MASTER_KEY_ID);
  if (!encoded) return null;
  const bytes = decodeBase64(encoded);
  if (bytes.length !== KEY_LEN) {
    log.warn(
      `stored master key has ${bytes.length} bytes; expected ${KEY_LEN}. ` +
        `Ignoring it and regenerating (any existing vault becomes unreadable).`,
    );
    return null;
  }
  return bytes;
}

async function loadOrCreateMasterKey(): Promise<CryptoKey> {
  if (cachedKey) return cachedKey;

  let raw = await readMasterKey();
  if (!raw) {
    raw = crypto.getRandomValues(new Uint8Array(KEY_LEN));
    await host().secureStore.set(MASTER_KEY_ID, encodeBase64(raw));
    log.info("generated a new vault master key");
  }

  cachedKey = await crypto.subtle.importKey(
    "raw",
    raw.buffer as ArrayBuffer,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"],
  );
  return cachedKey;
}

async function readEncrypted(): Promise<Record<string, string>> {
  const file = enginePaths().secretsEncFile;
  if (!(await host().fs.stat(file))) return {};
  let blob: Uint8Array;
  try {
    blob = await host().fs.readFile(file);
  } catch (err) {
    throw new AppError("internal_error", `failed to read secrets.enc: ${errMessage(err)}`);
  }
  if (blob.byteLength <= NONCE_LEN) return {};
  const key = await loadOrCreateMasterKey();
  // Slice the Uint8Array views into their own buffers. We can't pass
  // `blob.subarray(...)` directly because `subarray()` shares the
  // underlying ArrayBuffer with `blob`; web-crypto reads `.buffer` and
  // sees the full file, producing "Initialization vector length not
  // supported" at decrypt time. We also can't pass the Uint8Array
  // directly because lib.dom's BufferSource = ArrayBufferView<ArrayBuffer>
  // which excludes Uint8Array<ArrayBufferLike>.
  const nonce = blob.slice(0, NONCE_LEN);
  const ciphertext = blob.slice(NONCE_LEN);
  let plaintext: ArrayBuffer;
  try {
    plaintext = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: nonce.buffer as ArrayBuffer },
      key,
      ciphertext.buffer as ArrayBuffer,
    );
  } catch (err) {
    throw new AppError(
      "internal_error",
      `secrets.enc decryption failed (master key mismatch?): ${errMessage(err)}`,
    );
  }
  const text = new TextDecoder().decode(plaintext);
  if (!text) return {};
  try {
    return JSON.parse(text) as Record<string, string>;
  } catch {
    throw new AppError("internal_error", `secrets.enc decrypted but contained invalid JSON`);
  }
}

async function writeEncrypted(bag: Record<string, string>): Promise<void> {
  const key = await loadOrCreateMasterKey();
  const nonce = crypto.getRandomValues(new Uint8Array(NONCE_LEN));
  const plaintext = new TextEncoder().encode(JSON.stringify(bag));
  const ciphertext = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: nonce.buffer as ArrayBuffer },
      key,
      plaintext.buffer as ArrayBuffer,
    ),
  );
  const out = new Uint8Array(nonce.byteLength + ciphertext.byteLength);
  out.set(nonce, 0);
  out.set(ciphertext, nonce.byteLength);

  // restrictPermissions: 0600 so the encrypted bag file is owner-only (it leaks
  // nothing without the master key, but there's no reason to expose it).
  await host().fs.writeFileAtomic(enginePaths().secretsEncFile, out, { restrictPermissions: true });
}

export async function getSecret(name: string): Promise<string | undefined> {
  const bag = await readEncrypted();
  return bag[name];
}

// Serialize every vault mutation. setSecret/deleteSecret/clearAllSecrets are
// read-modify-write cycles over the single secrets.enc blob with an await
// between read and rename, so without serialization two concurrent writers
// (e.g. two paired clients saving secrets at once) both read the same bag and
// the second rename clobbers the first - a silent lost update. Chaining each
// mutation on one promise guarantees it reads the latest committed bag.
let vaultWriteChain: Promise<unknown> = Promise.resolve();
function withVaultLock<T>(fn: () => Promise<T>): Promise<T> {
  const run = vaultWriteChain.then(fn, fn);
  vaultWriteChain = run.then(
    () => {},
    () => {},
  );
  return run;
}

export async function setSecret(name: string, value: string): Promise<void> {
  if (!name || typeof name !== "string") {
    throw new AppError("validation_error", "secret name must be a non-empty string");
  }
  if (typeof value !== "string") {
    throw new AppError("validation_error", "secret value must be a string");
  }
  await withVaultLock(async () => {
    const bag = await readEncrypted();
    bag[name] = value;
    await writeEncrypted(bag);
    notifySecretsChanged(Object.keys(bag).sort());
  });
}

export function deleteSecret(name: string): Promise<boolean> {
  return withVaultLock(async () => {
    const bag = await readEncrypted();
    if (!(name in bag)) return false;
    delete bag[name];
    await writeEncrypted(bag);
    notifySecretsChanged(Object.keys(bag).sort());
    return true;
  });
}

export async function listSecretNames(): Promise<string[]> {
  const bag = await readEncrypted();
  return Object.keys(bag).sort();
}

/** Remove the entire encrypted vault (every stored secret). Used by the Storage
 *  view's "clear settings" factory reset. The stored master key is harmless
 *  without ciphertext, so we just delete the file. Missing-file tolerant. */
export function clearAllSecrets(): Promise<void> {
  return withVaultLock(async () => {
    await host().fs.remove(enginePaths().secretsEncFile);
    cachedKey = null;
    notifySecretsChanged([]);
  });
}

/** Boot-time integrity check (NON-mutating: never generates a key). If a sealed
 *  vault exists but no master key can be found, the stored secrets can't be
 *  decrypted - surface that at startup so the operator can restore the key
 *  instead of hitting an opaque failure mid-request. Common in dev when a
 *  rebuild drops the key but leaves secrets.enc behind. */
export async function warnIfVaultUnreadable(): Promise<void> {
  const file = enginePaths().secretsEncFile;
  if (!(await host().fs.stat(file))) return; // no vault
  const blob = await host().fs.readFile(file);
  if (blob.byteLength <= NONCE_LEN) return; // empty vault, nothing sealed
  if (await readMasterKey()) return; // key present: vault is readable
  log.warn(
    `secrets.enc exists but no master key was found. Stored secrets can't be ` +
      `decrypted; restore the master key or re-enter your secrets in Settings. ` +
      `In dev, preserve the master key across rebuilds (see ` +
      `packages/loca-core/README.md).`,
  );
}

function decodeBase64(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function encodeBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}
