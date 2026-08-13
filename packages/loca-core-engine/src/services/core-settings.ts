// Sparse settings file at ~/.loca/core/settings.json. Only non-default
// values are serialized; defaults come from the schema in @loca/shared.
//
// This module is intentionally untyped at the value layer. The schema in
// @loca/shared/domain/settings.ts owns field IDs and defaults, and the
// route layer typechecks PATCH bodies against it. Here we just persist a
// JSON record atomically.

import { errMessage, getDefaultSettings } from "@loca/shared";
import { host } from "../platform/runtime.ts";
import { enginePaths } from "../platform/paths.ts";
import { db } from "../platform/db.ts";
import { AppError } from "../platform/errors.ts";
import { getLogger } from "../platform/log.ts";

const log = getLogger("core-settings");

let cached: Record<string, unknown> | null = null;

// Serialize the shared-file read-modify-write. Without this, two concurrent
// PATCHes each read the same base and the second write clobbers the first's
// change on disk and in `cached` (a lost update). Mirrors withMemLock /
// withSessionLock, which added exactly this guarantee for their stores.
let settingsWriteChain: Promise<unknown> = Promise.resolve();
function withSettingsLock<T>(fn: () => Promise<T>): Promise<T> {
  const run = settingsWriteChain.then(fn, fn);
  settingsWriteChain = run.then(
    () => {},
    () => {},
  );
  return run;
}

// Per-client overlay cache: client id -> its sparse "client-on-core" settings
// (only the keys that client changed from the core-global defaults). Mirrors
// `cached` but partitioned by client. Populated lazily from the
// `client_settings` table; replaced on patch, dropped on client revoke.
const clientCache = new Map<string, Record<string, unknown>>();

// Test-only: drops the in-memory cache so the next `loadCoreSettings()`
// re-reads from disk. Also clears subscribers; test setup wires its own.
export function __resetForTesting(): void {
  cached = null;
  clientCache.clear();
  listeners.clear();
  clientListeners.clear();
}

export type SettingsListener = (
  settings: Record<string, unknown>,
  changedKeys: ReadonlySet<string>,
) => void | Promise<void>;

const listeners = new Set<SettingsListener>();

/** Fired after a per-client overlay PATCH with just the keys that changed for
 *  that one client (so the hub can deliver `settings.updated` to that client
 *  alone, never broadcasting another client's preferences). */
export type ClientSettingsListener = (
  clientId: string,
  values: Record<string, unknown>,
  deleted: string[],
) => void | Promise<void>;

const clientListeners = new Set<ClientSettingsListener>();

export async function loadCoreSettings(): Promise<Record<string, unknown>> {
  if (cached) return cached;
  const file = enginePaths().settingsFile;
  if (!(await host().fs.stat(file))) {
    cached = {};
    return cached;
  }
  const text = await host().fs.readTextFile(file);
  const parsed = JSON.parse(text);
  if (!parsed || typeof parsed !== "object") {
    throw new AppError("internal_error", "settings file is not an object");
  }
  cached = parsed as Record<string, unknown>;
  return cached;
}

/** Settings with schema defaults applied; persisted (sparse) values win. The
 *  on-disk file stores only non-default values, so any consumer that needs the
 *  effective configuration - not just the keys the user explicitly changed -
 *  must read through this. Requirement computation relies on it (e.g. the
 *  default model path / `llm.supportImages` aren't persisted when unchanged). */
export async function loadCoreSettingsResolved(): Promise<Record<string, unknown>> {
  return { ...getDefaultSettings(), ...(await loadCoreSettings()) };
}

export async function patchCoreSettings(
  partial: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  // Load -> merge -> write -> cache is one critical section: a concurrent PATCH
  // must not read the same base and clobber this write (lost update).
  const { merged, changed } = await withSettingsLock(async () => {
    const current = await loadCoreSettings();
    const merged: Record<string, unknown> = { ...current };
    const changed = new Set<string>();
    for (const [k, v] of Object.entries(partial)) {
      if (v === null || v === undefined) {
        if (k in merged) {
          delete merged[k];
          changed.add(k);
        }
      } else if (!Object.is(merged[k], v)) {
        merged[k] = v;
        changed.add(k);
      }
    }
    await writeAtomic(merged);
    cached = merged;
    return { merged, changed };
  });
  if (changed.size > 0) {
    for (const l of listeners) {
      try {
        await l(merged, changed);
      } catch (err) {
        // Listeners are fire-and-forget; surface log but don't block PATCH.
        log.error(`settings listener error: ${errMessage(err)}`);
      }
    }
  }
  return merged;
}

/** Reset all settings to their schema defaults: the sparse on-disk file becomes
 *  empty (`{}` means "every value is the default") and listeners fire with every
 *  previously-set key marked changed. Used by the Storage view's "clear
 *  settings" (factory reset). Secrets are wiped separately by the caller. */
export async function resetCoreSettings(): Promise<void> {
  const changed = await withSettingsLock(async () => {
    const previous = await loadCoreSettings();
    const keys = new Set(Object.keys(previous));
    await writeAtomic({});
    cached = {};
    return keys;
  });
  if (changed.size > 0) {
    // Every value is now a default; listeners get the empty (reset) settings.
    const reset: Record<string, unknown> = {};
    for (const l of listeners) {
      try {
        await l(reset, changed);
      } catch (err) {
        log.error(`settings listener error: ${errMessage(err)}`);
      }
    }
  }
}

/** Subscribe to settings changes. Listener fires AFTER the atomic write
 *  with the full merged settings + a set of just-changed keys. */
export function subscribeCoreSettings(listener: SettingsListener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

// --- per-client overlay (client-on-core destination) ----------------------

interface ClientSettingRow {
  key: string;
  value_json: string;
}

/** This client's sparse overlay: only the "client-on-core" keys it set, keyed
 *  by setting id. Read once from the DB then cached. */
function loadClientOverlay(clientId: string): Record<string, unknown> {
  const hit = clientCache.get(clientId);
  if (hit) return hit;
  const rows = db()
    .prepare("SELECT key, value_json FROM client_settings WHERE client_id = ?")
    .all(clientId) as ClientSettingRow[];
  const overlay: Record<string, unknown> = {};
  for (const row of rows) {
    try {
      overlay[row.key] = JSON.parse(row.value_json);
    } catch (err) {
      log.warn(`dropping corrupt client_settings value for ${row.key}: ${errMessage(err)}`);
    }
  }
  clientCache.set(clientId, overlay);
  return overlay;
}

/** The effective sparse settings the core should apply for a turn owned by
 *  `clientId`: the shared core-global values overlaid with that client's
 *  per-client overrides (overlay wins). Still sparse - downstream readers
 *  (boolSetting/numSetting/strSetting) apply the schema defaults. Every chat
 *  turn and automated session reads through this so the core honors each
 *  client's own inference preferences. */
export async function loadEffective(clientId: string): Promise<Record<string, unknown>> {
  const base = await loadCoreSettings();
  return { ...base, ...loadClientOverlay(clientId) };
}

/** Apply a sparse patch to one client's overlay (null/undefined deletes a key,
 *  reverting it to the core-global value). Returns the keys that actually
 *  changed, split into set values and deletions, for the WS delta. */
export async function patchClientSettings(
  clientId: string,
  partial: Record<string, unknown>,
): Promise<{ values: Record<string, unknown>; deleted: string[] }> {
  const overlay = { ...loadClientOverlay(clientId) };
  const values: Record<string, unknown> = {};
  const deleted: string[] = [];
  const database = db();
  const upsert = database.prepare(
    "INSERT INTO client_settings (client_id, key, value_json) VALUES (?, ?, ?) " +
      "ON CONFLICT(client_id, key) DO UPDATE SET value_json = excluded.value_json",
  );
  const del = database.prepare("DELETE FROM client_settings WHERE client_id = ? AND key = ?");
  // All-or-nothing: the per-key upserts/deletes run in one transaction so a
  // failure partway can't leave the overlay half-applied on disk while the
  // cache (updated only after COMMIT below) reflects a different state. The loop
  // is fully synchronous (no await), so the single shared connection never
  // interleaves this transaction with another.
  database.exec("BEGIN");
  try {
    for (const [k, v] of Object.entries(partial)) {
      if (v === null || v === undefined) {
        if (k in overlay) {
          del.run(clientId, k);
          delete overlay[k];
          deleted.push(k);
        }
      } else if (!Object.is(overlay[k], v)) {
        upsert.run(clientId, k, JSON.stringify(v));
        overlay[k] = v;
        values[k] = v;
      }
    }
    database.exec("COMMIT");
  } catch (err) {
    database.exec("ROLLBACK");
    // Drop the cache so the next read reloads the rolled-back DB truth instead of
    // the partially-mutated overlay we built above.
    clientCache.delete(clientId);
    throw err;
  }
  clientCache.set(clientId, overlay);
  if (Object.keys(values).length > 0 || deleted.length > 0) {
    for (const l of clientListeners) {
      try {
        await l(clientId, values, deleted);
      } catch (err) {
        log.error(`client settings listener error: ${errMessage(err)}`);
      }
    }
  }
  return { values, deleted };
}

/** Drop a client's overlay cache entry (the DB rows are reaped by the
 *  ON DELETE CASCADE from `clients`). Call when a client is removed/revoked. */
export function dropClientSettingsCache(clientId: string): void {
  clientCache.delete(clientId);
}

/** Wipe every client's per-client overlay. Part of the core-wide factory reset:
 *  the shared store and secrets are cleared alongside this, so the per-client
 *  inference overrides must go too, or "clear settings" would leave each client's
 *  agent personality (system prompt, sampling, tool/memory selection) intact.
 *  Fires `clientListeners` with the just-deleted keys per affected client so a
 *  connected client re-baselines to defaults without reconnecting. */
export async function resetAllClientSettings(): Promise<void> {
  const database = db();
  const rows = database.prepare("SELECT client_id, key FROM client_settings").all() as {
    client_id: string;
    key: string;
  }[];
  database.exec("DELETE FROM client_settings");
  clientCache.clear();
  if (rows.length === 0) return;
  const byClient = new Map<string, string[]>();
  for (const { client_id, key } of rows) {
    const list = byClient.get(client_id) ?? [];
    list.push(key);
    byClient.set(client_id, list);
  }
  for (const [clientId, deleted] of byClient) {
    for (const l of clientListeners) {
      try {
        await l(clientId, {}, deleted);
      } catch (err) {
        log.error(`client settings listener error: ${errMessage(err)}`);
      }
    }
  }
}

/** Subscribe to per-client overlay changes (one client's keys at a time). */
export function subscribeClientSettings(listener: ClientSettingsListener): () => void {
  clientListeners.add(listener);
  return () => clientListeners.delete(listener);
}

async function writeAtomic(value: Record<string, unknown>): Promise<void> {
  await host().fs.writeTextFileAtomic(enginePaths().settingsFile, JSON.stringify(value, null, 2));
}
