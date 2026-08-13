import { assertEquals } from "@std/assert";
import type { HostDb } from "@loca/core-engine";
import { openDenoDb } from "./deno-sqlite.ts";

// Parity between the @db/sqlite adapter and the engine's HostDb/HostStmt surface.
// The engine and every db() call site are typed against HostDb only, so these
// tests pin the exact behaviour a mobile HostDb adapter must also satisfy.

function freshDb(): HostDb {
  const db = openDenoDb(":memory:");
  db.exec("PRAGMA foreign_keys = ON");
  db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, n INTEGER)");
  return db;
}

Deno.test("exec + prepare/run/get/all round-trip", () => {
  const db = freshDb();
  const changes = db.prepare("INSERT INTO t (name, n) VALUES (?, ?)").run("a", 1);
  assertEquals(changes, 1);
  db.prepare("INSERT INTO t (name, n) VALUES (?, ?)").run("b", 2);

  const row = db
    .prepare("SELECT name, n FROM t WHERE name = ?")
    .get<{ name: string; n: number }>("a");
  assertEquals(row, { name: "a", n: 1 });

  const rows = db.prepare("SELECT name FROM t ORDER BY n").all<{ name: string }>();
  assertEquals(rows, [{ name: "a" }, { name: "b" }]);
  db.close();
});

Deno.test("get returns undefined for no match", () => {
  const db = freshDb();
  const row = db.prepare("SELECT * FROM t WHERE id = ?").get(999);
  assertEquals(row, undefined);
  db.close();
});

Deno.test("run returns the affected-row count", () => {
  const db = freshDb();
  db.prepare("INSERT INTO t (name, n) VALUES ('x', 1), ('y', 1), ('z', 2)").run();
  const changed = db.prepare("UPDATE t SET name = 'z' WHERE n = ?").run(1);
  assertEquals(changed, 2);
  db.close();
});

Deno.test("int64: INTEGER past 2^53 round-trips as bigint (not truncated)", () => {
  const db = freshDb();
  // 2^53 + 1 is not exactly representable as a JS number; int64: true must
  // return it as a bigint so ms timestamps / rowids past 2^53 survive.
  const big = 9007199254740993n; // 2^53 + 1
  db.prepare("INSERT INTO t (id, name, n) VALUES (?, ?, ?)").run(big, "big", 0);
  const row = db.prepare("SELECT id FROM t WHERE name = 'big'").get<{ id: bigint }>();
  assertEquals(row?.id, big);
  db.close();
});
