//! The configured list of servers: the `mcp_servers` table and nothing else.
//!
//! Rows hold the connection config and the user's per-tool and per-prompt
//! enablement. The live status and the capability counts come from the
//! [`manager`](super::manager), and the two are merged into the wire shape by
//! [`Mcp`](super::Mcp), which is the one place that holds both, so neither half
//! imports the other.
//!
//! A credential never reaches this table. A bearer token is recorded as the
//! `has_auth` flag and an OAuth grant as `oauth_authorized`; both values live in
//! the vault under the keys in [`secret_key`](super::secret_key).

use rusqlite::{Connection, OptionalExtension};

use crate::db::Db;
use crate::error::{validation, AppError, ErrorCode, Result};

use crate::contract::api::{McpRemoteAuth, McpStdioRuntime, McpTransportKind};

/// One stored server, typed. The config half of the wire `McpServer`; the live
/// half is the manager's.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub id: String,
    pub name: String,
    pub kind: McpTransportKind,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub runtime: McpStdioRuntime,
    pub deno_allow_all: bool,
    pub deno_permissions: Vec<String>,
    pub url: Option<String>,
    pub remote_auth: McpRemoteAuth,
    pub has_auth: bool,
    pub oauth_authorized: bool,
    pub enabled: bool,
    pub tool_enabled: Vec<String>,
    pub prompt_enabled: Vec<String>,
    /// Tool names the user turned always-available OFF for. An opt-OUT set:
    /// absent means the tool is always-available, which is the default.
    pub tool_always_available_off: Vec<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl ServerConfig {
    /// Whether this server should hold a live connection.
    pub fn wants_connection(&self) -> bool {
        self.enabled
    }
}

/// A new server, as the route hands it over. `has_auth` and `oauth_authorized`
/// are written by the routes that own the vault side, never inferred here.
///
/// `Default` is written out rather than derived: `McpTransportKind` has none,
/// and stdio is the transport the form starts on.
#[derive(Clone, Debug)]
pub struct NewServer {
    pub name: String,
    pub kind: McpTransportKind,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub runtime: Option<McpStdioRuntime>,
    pub deno_allow_all: Option<bool>,
    pub deno_permissions: Option<Vec<String>>,
    pub url: Option<String>,
    pub remote_auth: Option<McpRemoteAuth>,
    pub enabled: Option<bool>,
    pub has_auth: Option<bool>,
    pub oauth_authorized: Option<bool>,
}

impl Default for NewServer {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: McpTransportKind::Stdio,
            command: None,
            args: None,
            runtime: None,
            deno_allow_all: None,
            deno_permissions: None,
            url: None,
            remote_auth: None,
            enabled: None,
            has_auth: None,
            oauth_authorized: None,
        }
    }
}

/// A partial update. Every field absent means "leave as stored".
#[derive(Clone, Debug, Default)]
pub struct ServerPatch {
    pub name: Option<String>,
    pub kind: Option<McpTransportKind>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub runtime: Option<McpStdioRuntime>,
    pub deno_allow_all: Option<bool>,
    pub deno_permissions: Option<Vec<String>>,
    pub url: Option<String>,
    pub remote_auth: Option<McpRemoteAuth>,
    pub enabled: Option<bool>,
    pub has_auth: Option<bool>,
    pub oauth_authorized: Option<bool>,
}

impl ServerPatch {
    /// Whether this patch touches anything the transport is built from.
    ///
    /// A rename alone leaves the live session alone; a change to the transport,
    /// the command, the URL or the auth has to drop it so the following resync
    /// reconnects with the new settings.
    pub fn changes_connection(&self) -> bool {
        self.kind.is_some()
            || self.command.is_some()
            || self.args.is_some()
            || self.runtime.is_some()
            || self.deno_allow_all.is_some()
            || self.deno_permissions.is_some()
            || self.url.is_some()
            || self.remote_auth.is_some()
            || self.has_auth.is_some()
    }
}

/// Which JSON set a toggle writes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleSet {
    Tool,
    Prompt,
    /// The always-available OPT-OUT set: turning always-available off ADDS the
    /// name, turning it on removes it.
    ToolAlwaysAvailableOff,
}

impl ToggleSet {
    fn column(self) -> &'static str {
        match self {
            Self::Tool => "tool_enabled_json",
            Self::Prompt => "prompt_enabled_json",
            Self::ToolAlwaysAvailableOff => "tool_always_available_off_json",
        }
    }
}

#[derive(Clone)]
pub struct Registry {
    db: Db,
}

impl Registry {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn list(&self) -> Result<Vec<ServerConfig>> {
        self.db.call(list_rows).await
    }

    pub async fn get(&self, id: &str) -> Result<Option<ServerConfig>> {
        let id = id.to_string();
        self.db.call(move |conn| get_row(conn, &id)).await
    }

    /// The 404 path every route but `list` takes.
    pub async fn get_or_err(&self, id: &str) -> Result<ServerConfig> {
        self.get(id).await?.ok_or_else(|| not_found(id))
    }

    pub async fn create(&self, input: NewServer) -> Result<ServerConfig> {
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(validation("MCP server name required"));
        }
        // Only an enabled server needs a connection target: a freshly added
        // server starts as a disabled draft with the command or url blank.
        if input.enabled == Some(true) {
            assert_connectable(input.kind, input.command.as_deref(), input.url.as_deref())?;
        }
        let id = ulid::Ulid::new().to_string();
        let now = now_ms();
        let row_id = id.clone();
        self.db
            .call(move |conn| insert_row(conn, &row_id, &name, input, now))
            .await?;
        self.get_or_err(&id).await
    }

    pub async fn update(&self, id: &str, patch: ServerPatch) -> Result<ServerConfig> {
        let current = self.get_or_err(id).await?;
        // The enable gate again, applied to the POST-update state, so a draft
        // cannot be flipped on while still incomplete.
        let next_enabled = patch.enabled.unwrap_or(current.enabled);
        if next_enabled {
            assert_connectable(
                patch.kind.unwrap_or(current.kind),
                patch.command.as_deref().or(current.command.as_deref()),
                patch.url.as_deref().or(current.url.as_deref()),
            )?;
        }
        let id_owned = id.to_string();
        let now = now_ms();
        self.db
            .call(move |conn| update_row(conn, &id_owned, patch, &current, now))
            .await?;
        self.get_or_err(id).await
    }

    /// No 404: deleting an unknown id is a no-op, so a double-click on Delete
    /// does not turn into an error the user has to read.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.db.call(move |conn| delete_row(conn, &id)).await
    }

    /// Add or remove `name` in one of the row's JSON sets. Idempotent: the sets
    /// are sets, so enabling twice is one entry.
    pub async fn toggle(
        &self,
        id: &str,
        set: ToggleSet,
        name: &str,
        present: bool,
    ) -> Result<ServerConfig> {
        let id_owned = id.to_string();
        let name = name.to_string();
        let now = now_ms();
        self.db
            .call(move |conn| toggle_in_set(conn, &id_owned, set, &name, present, now))
            .await?;
        self.get_or_err(id).await
    }
}

pub fn not_found(id: &str) -> AppError {
    AppError::new(ErrorCode::NotFound, format!("MCP server {id} not found"))
}

/// A server can only connect with a transport target: a `command` for stdio, a
/// `url` for remote. Checked when a server is enabled rather than on every
/// save, so a disabled draft may keep both blank.
fn assert_connectable(
    kind: McpTransportKind,
    command: Option<&str>,
    url: Option<&str>,
) -> Result<()> {
    match kind {
        McpTransportKind::Stdio if blank(command) => {
            Err(validation("stdio MCP server needs a command"))
        }
        McpTransportKind::Remote if blank(url) => Err(validation("remote MCP server needs a url")),
        _ => Ok(()),
    }
}

fn blank(value: Option<&str>) -> bool {
    value.map(|v| v.trim().is_empty()).unwrap_or(true)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// --- sql ---------------------------------------------------------------------

const COLUMNS: &str = "id, name, kind, command, args_json, runtime, deno_allow_all, \
     deno_permissions_json, url, remote_auth, enabled, has_auth, oauth_authorized, \
     tool_enabled_json, prompt_enabled_json, tool_always_available_off_json, \
     created_at_ms, updated_at_ms";

fn list_rows(conn: &mut Connection) -> Result<Vec<ServerConfig>> {
    // COLLATE NOCASE so the client's alphabetical list does not put every
    // capitalised name first.
    let sql = format!("SELECT {COLUMNS} FROM mcp_servers ORDER BY name COLLATE NOCASE ASC");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map((), map_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn get_row(conn: &mut Connection, id: &str) -> Result<Option<ServerConfig>> {
    let sql = format!("SELECT {COLUMNS} FROM mcp_servers WHERE id = ?");
    let row = conn
        .prepare_cached(&sql)?
        .query_row((id,), map_row)
        .optional()?;
    Ok(row)
}

fn insert_row(
    conn: &mut Connection,
    id: &str,
    name: &str,
    input: NewServer,
    now: i64,
) -> Result<()> {
    // `tool_always_available_off_json` is deliberately not in the column list:
    // it takes the table's `'[]'` default, which is the empty opt-out set.
    conn.prepare_cached(
        "INSERT INTO mcp_servers (id, name, kind, command, args_json, runtime, deno_allow_all,
                                  deno_permissions_json, url, remote_auth, enabled, has_auth,
                                  oauth_authorized, tool_enabled_json, prompt_enabled_json,
                                  created_at_ms, updated_at_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '[]', '[]', ?, ?)",
    )?
    .execute(rusqlite::params![
        id,
        name,
        kind_str(input.kind),
        input.command,
        json_list(&input.args.unwrap_or_default()),
        runtime_str(input.runtime.unwrap_or(McpStdioRuntime::Custom)),
        // The default is ON: a deno-runtime server runs with --allow-all unless
        // the user opted into a manual permission list.
        flag(input.deno_allow_all.unwrap_or(true)),
        json_list(&input.deno_permissions.unwrap_or_default()),
        input.url,
        remote_auth_str(input.remote_auth.unwrap_or(McpRemoteAuth::None)),
        flag(input.enabled.unwrap_or(false)),
        flag(input.has_auth.unwrap_or(false)),
        flag(input.oauth_authorized.unwrap_or(false)),
        now,
        now,
    ])?;
    Ok(())
}

fn update_row(
    conn: &mut Connection,
    id: &str,
    patch: ServerPatch,
    current: &ServerConfig,
    now: i64,
) -> Result<()> {
    // An all-whitespace name keeps the stored one rather than blanking the
    // server in the list.
    let name = patch
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&current.name)
        .to_string();
    conn.prepare_cached(
        "UPDATE mcp_servers
            SET name = ?, kind = ?, command = ?, args_json = ?, runtime = ?, deno_allow_all = ?,
                deno_permissions_json = ?, url = ?, remote_auth = ?, enabled = ?, has_auth = ?,
                oauth_authorized = ?, updated_at_ms = ?
          WHERE id = ?",
    )?
    .execute(rusqlite::params![
        name,
        kind_str(patch.kind.unwrap_or(current.kind)),
        patch.command.clone().or_else(|| current.command.clone()),
        json_list(patch.args.as_ref().unwrap_or(&current.args)),
        runtime_str(patch.runtime.unwrap_or(current.runtime)),
        flag(patch.deno_allow_all.unwrap_or(current.deno_allow_all)),
        json_list(
            patch
                .deno_permissions
                .as_ref()
                .unwrap_or(&current.deno_permissions)
        ),
        patch.url.clone().or_else(|| current.url.clone()),
        remote_auth_str(patch.remote_auth.unwrap_or(current.remote_auth)),
        flag(patch.enabled.unwrap_or(current.enabled)),
        flag(patch.has_auth.unwrap_or(current.has_auth)),
        flag(patch.oauth_authorized.unwrap_or(current.oauth_authorized)),
        now,
        id,
    ])?;
    Ok(())
}

fn delete_row(conn: &mut Connection, id: &str) -> Result<()> {
    conn.prepare_cached("DELETE FROM mcp_servers WHERE id = ?")?
        .execute((id,))?;
    Ok(())
}

fn toggle_in_set(
    conn: &mut Connection,
    id: &str,
    set: ToggleSet,
    name: &str,
    present: bool,
    now: i64,
) -> Result<()> {
    let column = set.column();
    let stored: Option<String> = conn
        .prepare_cached(&format!("SELECT {column} FROM mcp_servers WHERE id = ?"))?
        .query_row((id,), |row| row.get(0))
        .optional()?;
    let Some(stored) = stored else {
        return Err(not_found(id));
    };
    let mut names = parse_list(&stored);
    let known = names.iter().any(|n| n == name);
    if present && !known {
        names.push(name.to_string());
    } else if !present {
        names.retain(|n| n != name);
    }
    conn.prepare_cached(&format!(
        "UPDATE mcp_servers SET {column} = ?, updated_at_ms = ? WHERE id = ?"
    ))?
    .execute(rusqlite::params![json_list(&names), now, id])?;
    Ok(())
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServerConfig> {
    Ok(ServerConfig {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: parse_kind(&row.get::<_, String>(2)?),
        command: row.get(3)?,
        args: parse_list(&row.get::<_, String>(4)?),
        runtime: parse_runtime(&row.get::<_, String>(5)?),
        deno_allow_all: row.get::<_, i64>(6)? != 0,
        deno_permissions: parse_list(&row.get::<_, String>(7)?),
        url: row.get(8)?,
        remote_auth: parse_remote_auth(&row.get::<_, String>(9)?),
        enabled: row.get::<_, i64>(10)? != 0,
        has_auth: row.get::<_, i64>(11)? != 0,
        oauth_authorized: row.get::<_, i64>(12)? != 0,
        tool_enabled: parse_list(&row.get::<_, String>(13)?),
        prompt_enabled: parse_list(&row.get::<_, String>(14)?),
        tool_always_available_off: parse_list(&row.get::<_, String>(15)?),
        created_at_ms: row.get(16)?,
        updated_at_ms: row.get(17)?,
    })
}

/// Every enum column is read leniently: a hand-edited or future-written value
/// falls back to the default rather than making the row unprojectable and the
/// whole list unreadable.
fn parse_kind(value: &str) -> McpTransportKind {
    match value {
        "remote" => McpTransportKind::Remote,
        _ => McpTransportKind::Stdio,
    }
}

fn parse_runtime(value: &str) -> McpStdioRuntime {
    match value {
        "deno" => McpStdioRuntime::Deno,
        _ => McpStdioRuntime::Custom,
    }
}

fn parse_remote_auth(value: &str) -> McpRemoteAuth {
    match value {
        "bearer" => McpRemoteAuth::Bearer,
        "oauth" => McpRemoteAuth::Oauth,
        _ => McpRemoteAuth::None,
    }
}

fn kind_str(kind: McpTransportKind) -> &'static str {
    match kind {
        McpTransportKind::Stdio => "stdio",
        McpTransportKind::Remote => "remote",
    }
}

fn runtime_str(runtime: McpStdioRuntime) -> &'static str {
    match runtime {
        McpStdioRuntime::Custom => "custom",
        McpStdioRuntime::Deno => "deno",
    }
}

fn remote_auth_str(auth: McpRemoteAuth) -> &'static str {
    match auth {
        McpRemoteAuth::None => "none",
        McpRemoteAuth::Bearer => "bearer",
        McpRemoteAuth::Oauth => "oauth",
    }
}

fn flag(value: bool) -> i64 {
    i64::from(value)
}

/// A stored JSON array of strings. Anything unparseable reads as empty rather
/// than failing the row.
fn parse_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn json_list(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
}
