//! Schema migrations, tracked via SQLite's `PRAGMA user_version`.
//!
//! v1 is the base schema (`schema.sql`, every `CREATE` an `IF NOT EXISTS`).
//! An additive change (`ALTER TABLE ADD COLUMN`, a new table or index) appends
//! an entry with the next version number and bumps [`CURRENT_SCHEMA_VERSION`];
//! each step then runs exactly once, in order, against a database whose stored
//! version is behind. This is what lets a self-update ship a schema change:
//! `CREATE TABLE IF NOT EXISTS` alone never adds a column to an existing table.
//!
//! `schema.sql` is an orchestrator-owned surface, so a schema change is a
//! request-queue entry plus a migration here, not an edit to the base schema.

use rusqlite::Connection;

use crate::error::{AppError, ErrorCode, Result};

/// The schema version this core build knows how to produce. Bump when adding a
/// [`MIGRATIONS`] entry.
pub const CURRENT_SCHEMA_VERSION: i32 = 1;

struct Migration {
    version: i32,
    sql: &'static str,
}

/// Ordered and append-only. Each entry's SQL runs once, when the database's
/// stored `user_version` is below `version`. Never edit or reorder a shipped
/// entry; add a new one. v1's body is the idempotent base schema, so
/// re-applying it to an existing v1 database is a no-op.
///
/// The schema is inlined rather than read at runtime so that a compiled binary
/// carries it with no path resolution.
const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: include_str!("../../schema.sql"),
}];

/// Bring `conn`'s schema up to [`CURRENT_SCHEMA_VERSION`].
pub fn migrate(conn: &Connection) -> Result<()> {
    let current = user_version(conn)?;

    if current > CURRENT_SCHEMA_VERSION {
        // A newer core wrote this database (an update rollback, or a channel
        // downgrade). This binary does not know the newer schema, so refuse
        // rather than risk misreading or corrupting it.
        return Err(AppError::new(
            ErrorCode::InternalError,
            format!(
                "database schema v{current} is newer than this core supports \
                 (v{CURRENT_SCHEMA_VERSION}); update loca-core to open it"
            ),
        ));
    }

    for m in MIGRATIONS {
        if m.version > current {
            // Not wrapped in a transaction, for two reasons. `schema.sql` opens
            // with `PRAGMA journal_mode = WAL`, which cannot run inside one, and
            // the Deno core does not wrap either, so wrapping would change the
            // failure mode of a partially applied migration against what an
            // existing install would have seen.
            //
            // `PRAGMA journal_mode = WAL` returns a row, and rusqlite's
            // `Statement::execute` rejects row-returning statements.
            // `execute_batch` is not built on it: it steps each statement
            // directly and tolerates a returned row. That is load-bearing here,
            // and `fresh_migrate_enables_wal` is the tripwire if it ever changes.
            conn.execute_batch(m.sql)?;
        }
    }

    if current < CURRENT_SCHEMA_VERSION {
        // PRAGMA does not accept bound parameters. The value is a compile-time
        // integer constant, never input.
        conn.execute_batch(&format!("PRAGMA user_version = {CURRENT_SCHEMA_VERSION}"))?;
    }

    Ok(())
}

/// The schema version stamped in the database file. 0 on one that has never
/// been migrated.
pub fn user_version(conn: &Connection) -> Result<i32> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::db::Db;
    use std::path::{Path, PathBuf};

    /// Every object in the schema, in a stable order. `sqlite_master.sql` stores
    /// the `CREATE` text verbatim, so comparing two of these is exact rather
    /// than structural. Implicit indexes (the ones SQLite builds for `UNIQUE`)
    /// carry a NULL `sql` and are included by name, since they are what enforces
    /// the constraint.
    fn schema_dump(conn: &Connection) -> String {
        let mut stmt = conn
            .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name")
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .unwrap();
        let mut out = String::new();
        for row in rows {
            let (kind, name, tbl, sql) = row.unwrap();
            out.push_str(&format!("[{kind} {name} on {tbl}]\n"));
            out.push_str(sql.as_deref().unwrap_or("(implicit)"));
            out.push_str("\n\n");
        }
        out
    }

    fn table_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    fn fresh_dump() -> String {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            migrate(conn)?;
            Ok(schema_dump(conn))
        })
        .unwrap()
    }

    fn testdata(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/db/testdata")
            .join(name)
    }

    // --- parity with the TypeScript core -------------------------------------

    /// `schema.sql` is a verbatim copy, and this is what keeps it one. An edit
    /// to either file fails here.
    ///
    /// The Deno core is deleted at Stage 5 cutover, taking its half of this
    /// comparison with it. `a_deno_written_database_migrates_to_a_no_op` is the
    /// guard that survives, because its fixture is a committed database rather
    /// than part of the TypeScript tree.
    #[test]
    fn schema_sql_matches_the_typescript_source() {
        let ts = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../loca-core/src/db/schema.sql")
            .canonicalize();
        let Ok(ts) = ts else {
            return;
        };
        let theirs = std::fs::read_to_string(&ts).unwrap();
        let ours = include_str!("../../schema.sql");
        assert_eq!(
            ours,
            theirs,
            "packages/loca-core-rs/schema.sql has drifted from {}",
            ts.display()
        );
    }

    /// The parity proof that matters: a database written by the Deno core opens
    /// as-is, needs no work, and migrates forward to exactly the schema a fresh
    /// Rust migrate produces.
    ///
    /// The fixture was produced by running the Deno core's `openDb()` +
    /// `migrate()` + `closeDb()` against a temporary `LOCA_CORE_HOME`, so it is
    /// a real `core.sqlite` with its WAL folded back in, not a reconstruction.
    /// It is a v1 database on purpose and never needs regenerating: adding v2
    /// makes this test prove the v1-to-v2 upgrade instead, which is the point.
    #[test]
    fn a_deno_written_database_migrates_to_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("core.sqlite");
        std::fs::copy(testdata("v1-deno.sqlite"), &path).unwrap();

        let db = Db::open(&path).unwrap();
        db.call_blocking(|conn| {
            assert_eq!(user_version(conn)?, 1, "fixture is not a v1 database");
            let before = schema_dump(conn);
            migrate(conn)?;
            let after = schema_dump(conn);
            assert_eq!(before, after, "migrate() modified an up-to-date database");
            assert_eq!(
                after,
                fresh_dump(),
                "a Deno-written database and a fresh Rust migrate disagree"
            );
            Ok(())
        })
        .unwrap();
    }

    // --- the version ladder ---------------------------------------------------

    /// Every historical `user_version` must reach the same final schema. With
    /// one migration this is two rungs and looks trivial; it is the harness that
    /// makes migration 2 provable on the day it is written.
    #[test]
    fn every_historical_version_migrates_to_the_same_schema() {
        let target = fresh_dump();
        for start in 0..=CURRENT_SCHEMA_VERSION {
            let db = Db::open_in_memory().unwrap();
            db.call_blocking(|conn| {
                for m in MIGRATIONS.iter().filter(|m| m.version <= start) {
                    conn.execute_batch(m.sql)?;
                }
                conn.execute_batch(&format!("PRAGMA user_version = {start}"))?;

                migrate(conn)?;

                assert_eq!(
                    schema_dump(conn),
                    target,
                    "migrating from v{start} produced a different schema"
                );
                assert_eq!(user_version(conn)?, CURRENT_SCHEMA_VERSION);
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn migrate_is_idempotent() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            migrate(conn)?;
            let before = schema_dump(conn);
            migrate(conn)?;
            assert_eq!(schema_dump(conn), before);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migrate_stamps_the_current_version() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            migrate(conn)?;
            assert_eq!(user_version(conn)?, CURRENT_SCHEMA_VERSION);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migrate_refuses_a_database_from_a_newer_core() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .call_blocking(|conn| {
                conn.execute_batch(&format!(
                    "PRAGMA user_version = {}",
                    CURRENT_SCHEMA_VERSION + 1
                ))?;
                migrate(conn)
            })
            .unwrap_err();
        assert!(
            err.message.contains("newer than this core supports"),
            "unexpected message: {}",
            err.message
        );
    }

    // --- what the schema must and must not contain ----------------------------

    /// The tripwire for `execute_batch` silently skipping the opening PRAGMA.
    /// A rollback-journal database reads fine and behaves differently under
    /// concurrency, so nothing else would surface it.
    #[test]
    fn fresh_migrate_enables_wal() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_migrated(&dir.path().join("core.sqlite")).unwrap();
        let mode: String = db
            .call_blocking(|conn| Ok(conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn the_canonical_tables_exist() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            migrate(conn)?;
            let names = table_names(conn);
            for expected in [
                "clients",
                "pairing_codes",
                "extensions",
                "tools",
                "tool_embeddings",
                "grants",
                "memories",
                "mcp_servers",
                "scheduled_prompts",
                "client_settings",
                "downloads",
            ] {
                assert!(
                    names.iter().any(|n| n == expected),
                    "missing table {expected}"
                );
            }
            Ok(())
        })
        .unwrap();
    }

    /// The half that matters. Sessions, messages, and attachments are JSON files
    /// under `<root>/sessions/<id>/`, deliberately, and this fails if anyone
    /// later helpfully moves them into SQLite.
    #[test]
    fn sessions_and_messages_are_not_tables() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            migrate(conn)?;
            let names = table_names(conn);
            for forbidden in ["sessions", "messages", "attachments"] {
                assert!(
                    !names.iter().any(|n| n == forbidden),
                    "{forbidden} is a table; sessions and messages stay JSON files on disk"
                );
            }
            Ok(())
        })
        .unwrap();
    }
}
