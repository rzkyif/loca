//! Every SQL statement this package issues.
//!
//! Kept in one file rather than inline in the service so the `clients` and
//! `pairing_codes` access is reviewable in one read, and so a change to F4's
//! connection handle is a single-file reconcile.
//!
//! Both tables are declared in `schema.sql`, which is orchestrator-owned. The
//! pairing code is stored in the clear on purpose: the PAKE keys off the code
//! value itself, so Core needs it for the code's lifetime, and `schema.sql` says
//! so.

use rusqlite::{Connection, OptionalExtension};

use crate::error::Result;

/// The single active (unclaimed) pairing code.
#[derive(Debug, Clone)]
pub(crate) struct CodeRow {
    pub(crate) id: i64,
    pub(crate) code: String,
    pub(crate) attempts: i64,
    pub(crate) expires_at_ms: i64,
}

/// A row of `clients`, as stored.
#[derive(Debug, Clone)]
pub(crate) struct ClientRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) created_at_ms: i64,
    pub(crate) last_seen_ms: i64,
    pub(crate) revoked: bool,
}

// --- pairing codes -----------------------------------------------------------

/// Drop every unclaimed code. Only one is ever live, so minting a new one wipes
/// whatever came before rather than leaving two codes that both open the door.
pub(crate) fn delete_unclaimed_codes(conn: &Connection) -> Result<()> {
    conn.prepare_cached("DELETE FROM pairing_codes WHERE claimed = 0")?
        .execute(())?;
    Ok(())
}

pub(crate) fn insert_code(
    conn: &Connection,
    code: &str,
    now_ms: i64,
    expires_at_ms: i64,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO pairing_codes (code, created_at_ms, expires_at_ms, attempts, claimed)
         VALUES (?, ?, ?, 0, 0)",
    )?
    .execute((code, now_ms, expires_at_ms))?;
    Ok(())
}

pub(crate) fn active_code(conn: &Connection) -> Result<Option<CodeRow>> {
    let row = conn
        .prepare_cached(
            "SELECT id, code, attempts, expires_at_ms FROM pairing_codes WHERE claimed = 0",
        )?
        .query_row((), |row| {
            Ok(CodeRow {
                id: row.get(0)?,
                code: row.get(1)?,
                attempts: row.get(2)?,
                expires_at_ms: row.get(3)?,
            })
        })
        .optional()?;
    Ok(row)
}

pub(crate) fn set_code_attempts(conn: &Connection, id: i64, attempts: i64) -> Result<()> {
    conn.prepare_cached("UPDATE pairing_codes SET attempts = ? WHERE id = ?")?
        .execute((attempts, id))?;
    Ok(())
}

/// Mark a code used. The same statement retires a claimed code and poisons one
/// that ran out of attempts: in both cases it stops being the active code.
pub(crate) fn claim_code(conn: &Connection, id: i64) -> Result<()> {
    conn.prepare_cached("UPDATE pairing_codes SET claimed = 1 WHERE id = ?")?
        .execute((id,))?;
    Ok(())
}

// --- clients -----------------------------------------------------------------

pub(crate) fn insert_client(
    conn: &Connection,
    id: &str,
    name: &str,
    token_hash: &str,
    now_ms: i64,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO clients (id, name, token_hash, created_at_ms, last_seen_ms, revoked)
         VALUES (?, ?, ?, ?, ?, 0)",
    )?
    .execute((id, name, token_hash, now_ms, now_ms))?;
    Ok(())
}

pub(crate) fn client_by_token_hash(
    conn: &Connection,
    token_hash: &str,
) -> Result<Option<ClientRow>> {
    let row = conn
        .prepare_cached(
            "SELECT id, name, created_at_ms, last_seen_ms, revoked
             FROM clients WHERE token_hash = ?",
        )?
        .query_row((token_hash,), read_client)
        .optional()?;
    Ok(row)
}

pub(crate) fn client_by_id(conn: &Connection, id: &str) -> Result<Option<ClientRow>> {
    let row = conn
        .prepare_cached(
            "SELECT id, name, created_at_ms, last_seen_ms, revoked
             FROM clients WHERE id = ?",
        )?
        .query_row((id,), read_client)
        .optional()?;
    Ok(row)
}

pub(crate) fn list_clients(conn: &Connection) -> Result<Vec<ClientRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, name, created_at_ms, last_seen_ms, revoked
         FROM clients ORDER BY created_at_ms DESC",
    )?;
    let rows = stmt.query_map((), read_client)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub(crate) fn update_last_seen(conn: &Connection, id: &str, now_ms: i64) -> Result<()> {
    conn.prepare_cached("UPDATE clients SET last_seen_ms = ? WHERE id = ?")?
        .execute((now_ms, id))?;
    Ok(())
}

/// Delete a client. Its `client_settings` rows and `scheduled_prompts` go with
/// it through `ON DELETE CASCADE`; the sessions on disk do not, which is what
/// the revocation hooks are for.
pub(crate) fn delete_client(conn: &Connection, id: &str) -> Result<bool> {
    let changed = conn
        .prepare_cached("DELETE FROM clients WHERE id = ?")?
        .execute((id,))?;
    Ok(changed > 0)
}

pub(crate) fn update_token_hash(conn: &Connection, id: &str, token_hash: &str) -> Result<()> {
    conn.prepare_cached("UPDATE clients SET token_hash = ? WHERE id = ?")?
        .execute((token_hash, id))?;
    Ok(())
}

fn read_client(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClientRow> {
    Ok(ClientRow {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at_ms: row.get(2)?,
        last_seen_ms: row.get(3)?,
        revoked: row.get::<_, i64>(4)? == 1,
    })
}
