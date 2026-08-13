// migrate() must be idempotent. Every CREATE in schema.sql uses
// IF NOT EXISTS. Running it twice against the same DB must not throw and
// must not change the schema.

import { assertEquals, assertThrows } from "@std/assert";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { db } from "@loca/core-engine";
import { CURRENT_SCHEMA_VERSION, migrate } from "./migrate.ts";

function tableNames(): string[] {
  return (
    db().prepare(`SELECT name FROM sqlite_master WHERE type='table' ORDER BY name`).all() as Array<{
      name: string;
    }>
  ).map((r) => r.name);
}

function userVersion(): number {
  const row = db().prepare("PRAGMA user_version").get() as {
    user_version?: number | bigint;
  };
  return Number(row?.user_version ?? 0);
}

Deno.test("migrate: running twice is idempotent (no throw, schema unchanged)", async () => {
  const env = await setupTestEnv();
  try {
    const before = tableNames();
    migrate();
    const after = tableNames();
    assertEquals(before, after);
    // Spot-check that the canonical tables exist after migration. Sessions /
    // messages / attachments are NOT in SQLite (they're JSON files on disk).
    assertEquals(before.includes("clients"), true);
    assertEquals(before.includes("pairing_codes"), true);
    assertEquals(before.includes("extensions"), true);
    assertEquals(before.includes("downloads"), true);
  } finally {
    await env.teardown();
  }
});

Deno.test("migrate: stamps the DB with the current schema version", async () => {
  const env = await setupTestEnv();
  try {
    // setupTestEnv already ran migrate(); the version must be stamped.
    assertEquals(userVersion(), CURRENT_SCHEMA_VERSION);
  } finally {
    await env.teardown();
  }
});

Deno.test("migrate: refuses a DB written by a newer core (forward-compat guard)", async () => {
  const env = await setupTestEnv();
  try {
    // Simulate an update rollback / channel downgrade: the on-disk DB carries a
    // schema version this older binary doesn't understand.
    db().exec(`PRAGMA user_version = ${CURRENT_SCHEMA_VERSION + 1}`);
    assertThrows(() => migrate(), Error, "newer than this core supports");
  } finally {
    await env.teardown();
  }
});
