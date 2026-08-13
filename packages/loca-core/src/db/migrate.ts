// Schema migration runner, tracked via SQLite's `PRAGMA user_version`.
//
// v1 is the base schema (schema.sql, all CREATE ... IF NOT EXISTS). Future
// additive changes (ALTER TABLE ADD COLUMN, new tables/indexes) append a new
// entry to MIGRATIONS with the next version number and bump
// CURRENT_SCHEMA_VERSION; each step runs exactly once, in order, on a DB whose
// stored user_version is behind. This is what lets a self-update ship a schema
// change: CREATE TABLE IF NOT EXISTS alone never adds a column to an existing
// table, so additive changes need real migration steps.

import { db } from "@loca/core-engine";

// schema.sql is loaded as a string at build time. Inlined via raw import to
// avoid runtime path resolution after `deno compile`.
import schemaSqlContent from "./schema.sql" with { type: "text" };

// The schema version this core build knows how to produce. Bump when adding a
// MIGRATIONS entry.
export const CURRENT_SCHEMA_VERSION = 1;

// Ordered, append-only. Each entry's SQL runs once when the DB's stored
// user_version is below `version`. Never edit or reorder a shipped entry; add a
// new one. v1's body is the idempotent base schema, so re-applying it to an
// existing v1 db is a no-op (every statement is IF NOT EXISTS).
const MIGRATIONS: ReadonlyArray<{ version: number; sql: string }> = [
  { version: 1, sql: schemaSqlContent },
];

export function migrate(): void {
  const database = db();
  const row = database.prepare("PRAGMA user_version").get() as
    | { user_version?: number | bigint }
    | undefined;
  const current = Number(row?.user_version ?? 0);

  if (current > CURRENT_SCHEMA_VERSION) {
    // A newer core wrote this DB (e.g. an update rollback / channel downgrade).
    // This older binary doesn't know the newer schema, so refuse rather than
    // risk misreading or corrupting it.
    throw new Error(
      `database schema v${current} is newer than this core supports ` +
        `(v${CURRENT_SCHEMA_VERSION}); update loca-core to open it`,
    );
  }

  for (const m of MIGRATIONS) {
    if (m.version > current) database.exec(m.sql);
  }

  if (current < CURRENT_SCHEMA_VERSION) {
    // PRAGMA does not accept bound parameters; the value is a trusted integer
    // constant, never user input.
    database.exec(`PRAGMA user_version = ${CURRENT_SCHEMA_VERSION}`);
  }
}
