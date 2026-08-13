// Desktop HostDb adapter: wraps jsr:@db/sqlite (native FFI SQLite) in the
// engine's runtime-agnostic HostDb / HostStmt surface. This is the ONE place in
// the Deno host that touches @db/sqlite; the engine and every db() call site see
// only HostDb, so a mobile host can swap in a WASM SQLite behind the same shape.
//
// Every connection is opened with int64: true (INTEGER columns round-trip values
// past 2^53 as bigint, e.g. Date.now() ms timestamps). Per-connection pragmas
// (foreign_keys, busy_timeout, WAL) are the caller's job via exec(), matching the
// previous direct-Database usage.

import { Database } from "@db/sqlite";
import type { HostDb, HostStmt, SqlBindValue } from "@loca/core-engine";

// The slice of @db/sqlite's Statement the adapter forwards to, with rows typed as
// `unknown`: a SQLite row is dynamic data, so the caller asserts its shape at
// get<T>() / all<T>() exactly as it did before against the native driver. Typing
// it here (rather than importing Statement's generics) keeps the `as T` at the
// boundary a truthful unknown-assertion, not a cast between two generic APIs.
interface RawStmt {
  get(...params: SqlBindValue[]): unknown;
  all(...params: SqlBindValue[]): unknown[];
  run(...params: SqlBindValue[]): number;
  value(...params: SqlBindValue[]): unknown;
  finalize(): void;
}

class DenoStmt implements HostStmt {
  readonly #stmt: RawStmt;

  constructor(stmt: RawStmt) {
    this.#stmt = stmt;
  }

  get<T = unknown>(...params: SqlBindValue[]): T | undefined {
    return this.#stmt.get(...params) as T | undefined;
  }
  all<T = unknown>(...params: SqlBindValue[]): T[] {
    return this.#stmt.all(...params) as T[];
  }
  run(...params: SqlBindValue[]): number {
    return this.#stmt.run(...params);
  }
  value<T = unknown>(...params: SqlBindValue[]): T | undefined {
    return this.#stmt.value(...params) as T | undefined;
  }
  finalize(): void {
    this.#stmt.finalize();
  }
}

class DenoDb implements HostDb {
  readonly #db: Database;

  constructor(absPath: string) {
    this.#db = new Database(absPath, { int64: true });
  }

  exec(sql: string): void {
    this.#db.exec(sql);
  }
  prepare(sql: string): HostStmt {
    return new DenoStmt(this.#db.prepare(sql));
  }
  close(): void {
    this.#db.close();
  }
}

/** Open a SQLite database at an absolute path as a HostDb. The directory must
 *  already exist (the caller creates it). */
export function openDenoDb(absPath: string): HostDb {
  return new DenoDb(absPath);
}
