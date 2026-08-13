// Single SQLite connection for the engine, opened through host.openDb so every
// db() call site sees the runtime-agnostic HostDb surface (desktop = @db/sqlite;
// a future mobile host = a WASM SQLite) rather than a driver directly. WAL is
// persisted in the db file; foreign_keys + busy_timeout are connection-level and
// set here so they hold for the one shared handle.

import type { HostDb } from "../host.ts";
import { host } from "./runtime.ts";
import { enginePaths } from "./paths.ts";

let handle: HostDb | null = null;

export function db(): HostDb {
  if (handle === null) {
    throw new Error("db() called before openDb()");
  }
  return handle;
}

export function openDb(): HostDb {
  if (handle !== null) return handle;
  handle = host().openDb(enginePaths().dbFile);
  // foreign_keys defaults to OFF and is NOT persisted, so it must be set per
  // connection rather than relying on schema.sql having run: a connection opened
  // without migrate() would otherwise silently lose ON DELETE CASCADE.
  // busy_timeout makes momentary lock contention retry instead of throwing.
  handle.exec("PRAGMA foreign_keys = ON");
  handle.exec("PRAGMA busy_timeout = 5000");
  return handle;
}

export function closeDb(): void {
  if (handle) {
    try {
      // Fold the WAL back into the main db file on clean shutdown so the -wal
      // doesn't grow across the process lifetime and the next open is clean.
      handle.exec("PRAGMA wal_checkpoint(TRUNCATE)");
    } catch {
      /* best-effort: a checkpoint failure must not block shutdown */
    }
    handle.close();
    handle = null;
  }
}
