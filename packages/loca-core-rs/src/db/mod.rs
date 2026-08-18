//! SQLite storage: the schema, PRAGMA user_version migrations, and connection handling.
//!
//! Work package **F4** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! # The handle
//!
//! One connection, behind a mutex, reached through [`Db::call`], which hands the
//! work to `spawn_blocking`. The closure is synchronous and the guard is taken
//! and dropped inside it, so a `MutexGuard` can never be held across an `.await`
//! and no handler can block a tokio worker thread on SQLite by accident.
//!
//! One connection rather than a pool because every query here is a small
//! metadata read or write (paired clients, tools, grants, memories metadata, the
//! sparse per-client settings overlay), so the concurrent readers WAL would buy
//! go unused, while the extra page cache per connection would be charged to the
//! mobile embed, where the core shares an address space and a memory ceiling
//! with the app. All database work therefore serializes, which is what the Deno
//! core did too, except that there a slow query blocked the whole event loop and
//! here it blocks only other database users.
//!
//! There is deliberately no global handle and no `db()` accessor. `cargo test`
//! runs tests as threads in one process, so a `OnceLock<Db>` would force every
//! database test in the crate onto one database. [`Db`] is a cheap `Arc` clone
//! instead, constructed at boot and carried in the application state (D-010).
//!
//! # Persistence parity
//!
//! `core.sqlite` is opened as-is: no re-create, no re-key, no `VACUUM`, no page
//! size or encoding change. `PRAGMA synchronous` is left at the SQLite default,
//! because setting it would be a silent durability change dressed up as a
//! performance win. Sessions, messages, and attachments are JSON files on disk,
//! not tables here; `schema.sql` says so and means it.

pub mod migrate;

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{ffi, Connection};

use crate::error::{AppError, ErrorCode, Result};

pub use migrate::{migrate, user_version, CURRENT_SCHEMA_VERSION};

/// Connection pragmas, applied at every open.
///
/// `foreign_keys` is not persisted in the database file, so it must be set per
/// connection rather than relied on from `schema.sql` having run: a connection
/// opened without it silently loses every `ON DELETE CASCADE`.
/// `busy_timeout` makes momentary lock contention retry instead of failing.
///
/// A trap worth naming, because it makes the failure invisible rather than
/// loud: stock SQLite defaults `foreign_keys` to OFF, but `libsqlite3-sys`
/// compiles the bundled amalgamation with `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`, so
/// in this build cascades fire whether or not this pragma runs. Dropping it
/// would therefore break nothing here and everything on a build that links a
/// system SQLite. `open_pragmas_turn_foreign_keys_back_on` tests the constant
/// against an explicitly-off connection so it cannot ride on the build default.
///
/// `journal_mode = WAL` is absent on purpose. It is a persisted property of the
/// database file and is applied by the v1 migration (`schema.sql`), so
/// re-asserting it here would be a second source of truth for it.
const OPEN_PRAGMAS: &str = "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;";

/// A handle to the core's SQLite database. Cloning is cheap and every clone
/// shares one connection.
#[derive(Clone, Debug)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// Open the database at `path` and apply the connection pragmas. Does not
    /// migrate: use [`Db::open_migrated`] for `core.sqlite`. Plain `open` is for
    /// the databases that carry no schema of ours, such as an extension's
    /// private `data.sqlite`.
    ///
    /// The path is a parameter rather than read from `paths` so that this module
    /// stays independent of the filesystem layout and testable against a
    /// tempdir; the two meet in the boot frontend.
    pub fn open(path: &Path) -> Result<Self> {
        Self::wrap(Connection::open(path)?)
    }

    /// Open `path`, apply the pragmas, and bring the schema up to
    /// [`CURRENT_SCHEMA_VERSION`]. This is how `core.sqlite` is opened.
    pub fn open_migrated(path: &Path) -> Result<Self> {
        let db = Self::open(path)?;
        db.call_blocking(|conn| migrate(conn))?;
        Ok(db)
    }

    /// An in-memory database with the pragmas applied. Tests only: it lives and
    /// dies with the handle, and `journal_mode = WAL` is a no-op against it.
    pub fn open_in_memory() -> Result<Self> {
        Self::wrap(Connection::open_in_memory()?)
    }

    fn wrap(conn: Connection) -> Result<Self> {
        conn.execute_batch(OPEN_PRAGMAS)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run `f` against the connection on the blocking pool. This is the way
    /// route handlers reach the database.
    ///
    /// The closure is `&mut Connection` because the guard already gives
    /// exclusive access and `Connection::transaction` needs a `&mut`. Prepared
    /// statements should go through `Connection::prepare_cached`, which is the
    /// per-connection statement cache and the equivalent of the Deno core
    /// reusing prepared statements against one long-lived handle.
    ///
    /// Concurrent calls each occupy a blocking-pool thread while parked on the
    /// mutex. tokio's pool defaults to 512 threads against a handful of paired
    /// clients running sub-millisecond queries, so this is bounded and cheap. If
    /// it ever stops being so, the escalation is a single dedicated database
    /// thread fed by a channel of boxed closures, which stays inside this module.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = lock(&conn);
            f(&mut guard)
        })
        .await
        .map_err(|err| {
            AppError::new(
                ErrorCode::InternalError,
                format!("database task failed: {err}"),
            )
        })?
    }

    /// Run `f` against the connection on the current thread.
    ///
    /// For boot (before the server is serving), shutdown, tests, and any caller
    /// with no ambient tokio runtime, such as a mobile FFI entry point. Route
    /// handlers use [`Db::call`]: this one blocks the thread it is called on, so
    /// reaching for it to avoid making a function `async` is exactly the mistake
    /// [`Db::call`] exists to prevent.
    pub fn call_blocking<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T>,
    {
        let mut guard = lock(&self.conn);
        f(&mut guard)
    }

    /// Fold the WAL back into the main database file, best-effort, so the `-wal`
    /// does not grow across the process lifetime and the next open is clean.
    ///
    /// Takes `&self` because other clones may still exist; the connection itself
    /// closes when the last one drops. A checkpoint failure must not block
    /// shutdown, so it is logged and swallowed.
    pub fn close(&self) {
        let guard = lock(&self.conn);
        if let Err(err) = guard.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)") {
            tracing::warn!(target: "loca::db", "wal checkpoint on close failed: {err}");
        }
    }
}

/// Take the connection lock, recovering from poisoning rather than propagating
/// it. A panicking query leaves the connection usable (rusqlite finalizes a
/// half-stepped statement on drop), and the release profile aborts on panic, so
/// a poisoned mutex is only ever reachable under test.
fn lock(conn: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
    conn.lock().unwrap_or_else(PoisonError::into_inner)
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        AppError::new(code_for(&err), format!("database error: {err}"))
    }
}

/// Map a SQLite failure onto the wire contract.
///
/// `DiskFull` matches the `From<std::io::Error>` mapping in `error.rs`, so a
/// full disk reaches the client's "free some space" path whether the write went
/// through a file or a table.
///
/// Only a duplicate key is a conflict the client can resolve, so the extended
/// code is checked rather than the primary `ConstraintViolation`: a `NOT NULL`
/// or `CHECK` violation is a bug in the core, and answering 409 would blame the
/// client for it.
///
/// `QueryReturnedNoRows` is internal rather than 404 on purpose. A missing row
/// is a domain decision, and call sites use `.optional()` so they never reach
/// this arm; a route wanting 404 returns `not_found` itself.
fn code_for(err: &rusqlite::Error) -> ErrorCode {
    let rusqlite::Error::SqliteFailure(inner, _) = err else {
        return ErrorCode::InternalError;
    };
    match inner.code {
        ffi::ErrorCode::DiskFull => ErrorCode::InsufficientStorage,
        ffi::ErrorCode::ConstraintViolation
            if matches!(
                inner.extended_code,
                ffi::SQLITE_CONSTRAINT_UNIQUE | ffi::SQLITE_CONSTRAINT_PRIMARYKEY
            ) =>
        {
            ErrorCode::Conflict
        }
        _ => ErrorCode::InternalError,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn migrated() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| migrate(conn)).unwrap();
        db
    }

    fn pragma(db: &Db, name: &str) -> i64 {
        db.call_blocking(
            |conn| Ok(conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))?),
        )
        .unwrap()
    }

    #[test]
    fn open_applies_the_connection_pragmas() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(pragma(&db, "foreign_keys"), 1);
        assert_eq!(pragma(&db, "busy_timeout"), 5000);
    }

    /// The real tripwire for the pragma string. Asserting `foreign_keys` after a
    /// plain open proves nothing, because the bundled build already defaults it
    /// on; starting from an explicitly-off connection is what pins that
    /// [`OPEN_PRAGMAS`] itself turns it back on.
    #[test]
    fn open_pragmas_turn_foreign_keys_back_on() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        let off: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(off, 0, "could not stage an explicitly-off connection");

        conn.execute_batch(OPEN_PRAGMAS).unwrap();
        let on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(on, 1, "OPEN_PRAGMAS no longer enables foreign_keys");
    }

    /// The schema's `ON DELETE CASCADE` clauses do what they say. This passes on
    /// the build default alone, so it is a behaviour assertion rather than the
    /// pragma tripwire above.
    #[test]
    fn on_delete_cascade_actually_fires() {
        let db = migrated();
        db.call_blocking(|conn| {
            conn.execute(
                "INSERT INTO clients (id, name, token_hash, created_at_ms, last_seen_ms)
                 VALUES ('c1', 'test', 'hash', 1, 1)",
                [],
            )?;
            conn.execute(
                "INSERT INTO client_settings (client_id, key, value_json)
                 VALUES ('c1', 'llm.temperature', '0.5')",
                [],
            )?;
            conn.execute("DELETE FROM clients WHERE id = 'c1'", [])?;
            let overlay: i64 =
                conn.query_row("SELECT count(*) FROM client_settings", [], |row| row.get(0))?;
            assert_eq!(overlay, 0, "client_settings row outlived its client");
            Ok(())
        })
        .unwrap();
    }

    #[tokio::test]
    async fn call_round_trips_a_value() {
        let db = Db::open_in_memory().unwrap();
        let n: i64 = db
            .call(|conn| Ok(conn.query_row("SELECT 41 + 1", [], |row| row.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, 42);
    }

    /// Concurrent calls must all complete. A guard held across an await would
    /// deadlock here instead.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_calls_all_complete() {
        let db = migrated();
        let mut tasks = Vec::new();
        for i in 0..32i64 {
            let db = db.clone();
            tasks.push(tokio::spawn(async move {
                db.call(move |conn| {
                    conn.execute(
                        "INSERT INTO clients (id, name, token_hash, created_at_ms, last_seen_ms)
                         VALUES (?1, 'test', ?2, 0, 0)",
                        rusqlite::params![format!("c{i}"), format!("hash{i}")],
                    )?;
                    Ok(())
                })
                .await
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        let count: i64 = db
            .call(|conn| Ok(conn.query_row("SELECT count(*) FROM clients", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(count, 32);
    }

    #[tokio::test]
    async fn call_propagates_a_closure_error() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .call(|_| Err::<(), _>(crate::error::conflict("nope")))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
    }

    #[test]
    fn duplicate_key_is_a_conflict() {
        let db = migrated();
        let err = db
            .call_blocking(|conn| {
                for _ in 0..2 {
                    conn.execute(
                        "INSERT INTO clients (id, name, token_hash, created_at_ms, last_seen_ms)
                         VALUES ('dup', 'test', 'same-hash', 0, 0)",
                        [],
                    )?;
                }
                Ok(())
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
    }

    /// A 500 from a constraint violation is deliberate, not an oversight.
    ///
    /// If you arrived here from a `NOT NULL` or `CHECK` violation surfacing as
    /// `internal_error`: that is the intended answer, because the client did
    /// nothing it could have done differently. Only a duplicate key
    /// (`SQLITE_CONSTRAINT_UNIQUE`, `SQLITE_CONSTRAINT_PRIMARYKEY`) is a
    /// conflict the client can resolve, so only those become 409. Answering 409
    /// for the rest would report a bug in the core as the caller's fault, and
    /// the caller would retry forever against a row it can never insert.
    ///
    /// A route that needs different semantics for a specific constraint maps it
    /// at the call site rather than widening this: `memories.title` and
    /// `memories.filename` (D5) and `tools` / `extensions` (D12) are the
    /// `UNIQUE` constraints that reach a user, and they already get 409 here.
    #[test]
    fn a_not_null_violation_is_internal_not_a_conflict() {
        let db = migrated();
        let err = db
            .call_blocking(|conn| {
                conn.execute("INSERT INTO clients (id) VALUES ('no-name')", [])?;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InternalError);
    }

    #[test]
    fn an_arbitrary_failure_is_internal() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .call_blocking(|conn| {
                conn.execute("SELECT nonsense FROM nowhere", [])?;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InternalError);
    }

    #[test]
    fn close_checkpoints_and_leaves_the_database_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("core.sqlite");
        let db = Db::open_migrated(&path).unwrap();
        db.call_blocking(|conn| {
            conn.execute(
                "INSERT INTO clients (id, name, token_hash, created_at_ms, last_seen_ms)
                 VALUES ('c1', 'test', 'hash', 1, 1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        db.close();

        let wal = path.with_extension("sqlite-wal");
        assert!(
            !wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0,
            "wal_checkpoint(TRUNCATE) left a non-empty -wal behind"
        );
        let count: i64 = db
            .call_blocking(|conn| {
                Ok(conn.query_row("SELECT count(*) FROM clients", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    /// The whole design rests on the handle being shareable across tasks.
    #[test]
    fn db_is_send_sync_and_clone() {
        fn assert_bounds<T: Send + Sync + Clone>() {}
        assert_bounds::<Db>();
    }
}
