//! The module broker: the gate a tool's `module_request` passes through, and
//! the containment screen on the SQL it may run.
//!
//! Dispatch reaches five other subsystems through the traits in
//! [`super::worker::call`], so this module imports no `memories::`, `llm::` or
//! `speech::` and the module graph stays acyclic.
//!
//! This is the non-PTY counterpart of the runtime permission prompt: an access
//! that is declared but not granted pauses the call on the SAME client prompt,
//! and the approval it gets back is scoped to that one call.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use base64::prelude::{Engine as _, BASE64_STANDARD};
use serde_json::{Map, Value};

use crate::contract::api::{
    Grant, GrantState, PermissionDecl, PermissionDeclAccess, PermissionKind, UndeclaredPolicy,
};
use crate::db::Db;
use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;
use crate::settings::{access::bool_setting, SettingsRecord};

use super::sandbox::{permission_key, permission_kind_of};
use super::worker::call::{ModuleCall, ModuleDispatch, ModulePrompt, ModulePrompter};
use super::worker::protocol::ModuleName;

/// A query returning more rows than this is refused rather than truncated: a
/// silently partial answer is worse than a visible error.
pub const SQL_QUERY_MAX_ROWS: usize = 10_000;

/// Everything the registry knows that the gate needs, for one (extension,
/// tool) pair. Passed in rather than looked up, so the gate stays a pure
/// decision over stated facts.
#[derive(Debug, Clone)]
pub struct GateFacts {
    /// The extension declared `"database": true`, which the user saw at
    /// install time.
    pub has_database: bool,
    pub undeclared_policy: UndeclaredPolicy,
    /// The invoked tool's declared permissions, as persisted at register time.
    pub required: Vec<PermissionDecl>,
    /// The grant rows for that tool.
    pub grants: Vec<Grant>,
}

/// Which half of the memories API an op touches. Parsed rather than defaulted:
/// an op that is not one of these is refused before the gate, so it can never
/// slip through under the weaker access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoriesAccess {
    Read,
    Write,
}

impl MemoriesAccess {
    pub fn of(op: &str) -> Option<Self> {
        MemoriesOp::of(op).map(MemoriesOp::access)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// The five memories ops. The gate and the dispatch read this one table, so
/// the access an op is gated at and the code that runs for it cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoriesOp {
    List,
    Get,
    GetFile,
    Write,
    Edit,
}

impl MemoriesOp {
    pub fn of(op: &str) -> Option<Self> {
        match op {
            "list" => Some(Self::List),
            "get" => Some(Self::Get),
            "getFile" => Some(Self::GetFile),
            "write" => Some(Self::Write),
            "edit" => Some(Self::Edit),
            _ => None,
        }
    }

    fn access(self) -> MemoriesAccess {
        match self {
            Self::List | Self::Get | Self::GetFile => MemoriesAccess::Read,
            Self::Write | Self::Edit => MemoriesAccess::Write,
        }
    }
}

/// Decide whether one module request may proceed.
///
/// `Ok(())` to dispatch. `Err` is always `forbidden`, and it is the only thing
/// standing between a tool's declared permission and an actual capability.
pub async fn gate(
    module: ModuleName,
    op: &str,
    facts: &GateFacts,
    prompter: &dyn ModulePrompter,
) -> Result<()> {
    // `db` is DECLARATION gated and only that: no grant rows, no prompt,
    // because there is no per-call decision to make.
    if module == ModuleName::Db {
        if !facts.has_database {
            return Err(forbidden(
                "this extension does not declare \"database\": true",
            ));
        }
        return Ok(());
    }

    let permission = match module {
        ModuleName::Memories => PermissionKind::Memories,
        ModuleName::Llm => PermissionKind::Llm,
        ModuleName::Tts => PermissionKind::Tts,
        ModuleName::Stt => PermissionKind::Stt,
        ModuleName::Db => unreachable!("handled above"),
    };
    let access = if module == ModuleName::Memories {
        // Refused here rather than gated under the weaker of the two accesses.
        Some(MemoriesAccess::of(op).ok_or_else(|| {
            AppError::new(
                ErrorCode::ValidationError,
                format!("unknown memories op \"{op}\""),
            )
        })?)
    } else {
        None
    };
    let resource = access.map(MemoriesAccess::as_str).unwrap_or("");

    let Some(decl) = facts
        .required
        .iter()
        .find(|decl| decl_covers(decl, permission, access))
    else {
        if facts.undeclared_policy == UndeclaredPolicy::Deny {
            return Err(forbidden(format!(
                "undeclared {} access denied by extension policy",
                kind_name(permission)
            )));
        }
        return ask(prompter, permission, resource, false, None).await;
    };

    let state = facts
        .grants
        .iter()
        .find(|grant| grant.permission_key == permission_key(decl))
        .map(|grant| grant.state)
        // No row means nobody has decided yet: `ask`, never a silent approval.
        .unwrap_or(GrantState::Ask);

    match state {
        GrantState::Denied => Err(forbidden(format!(
            "{} access denied by grant",
            kind_name(permission)
        ))),
        GrantState::Granted => Ok(()),
        GrantState::Ask => ask(prompter, permission, resource, true, decl_reason(decl)).await,
    }
}

async fn ask(
    prompter: &dyn ModulePrompter,
    permission: PermissionKind,
    resource: &str,
    declared: bool,
    reason: Option<String>,
) -> Result<()> {
    let allowed = prompter
        .ask(ModulePrompt {
            permission,
            resource: resource.to_string(),
            declared,
            reason,
        })
        .await;
    if allowed {
        // Deliberately NOT written to the grants table: the approval covers
        // this request only. "Always allow" lives in the extension detail view.
        return Ok(());
    }
    Err(forbidden(format!(
        "{} access rejected by user",
        kind_name(permission)
    )))
}

/// Does this declaration cover the access being attempted? A `memories: write`
/// declaration covers a read; the reverse is NOT true, and that asymmetry is
/// the whole content of this function.
fn decl_covers(
    decl: &PermissionDecl,
    permission: PermissionKind,
    access: Option<MemoriesAccess>,
) -> bool {
    if permission_kind_of(decl) != permission {
        return false;
    }
    let PermissionDecl::Memories {
        access: declared, ..
    } = decl
    else {
        return true;
    };
    matches!(
        (declared, access),
        (PermissionDeclAccess::Write, Some(_))
            | (PermissionDeclAccess::Read, Some(MemoriesAccess::Read))
    )
}

fn forbidden(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::Forbidden, message)
}

fn kind_name(kind: PermissionKind) -> String {
    format!("{kind:?}").to_lowercase()
}

fn decl_reason(decl: &PermissionDecl) -> Option<String> {
    match decl {
        PermissionDecl::Memories { reason, .. }
        | PermissionDecl::Llm { reason, .. }
        | PermissionDecl::Tts { reason, .. }
        | PermissionDecl::Stt { reason, .. } => Some(reason.clone()),
        _ => None,
    }
}

// --- the SQL containment screen ------------------------------------------

/// Refuse SQL that would reach outside the extension's own database file.
/// Extension SQL runs under Core's privileges, so `ATTACH`/`DETACH` would open
/// arbitrary database files (`core.sqlite` among them) and `VACUUM INTO` writes
/// one to an arbitrary path. Lexical, over [`strip_sql_literals`] output, so a
/// row of data is never mistaken for a statement.
pub fn assert_sql_allowed(sql: &str) -> Result<()> {
    let screened = strip_sql_literals(sql).to_uppercase();
    if has_word(&screened, "ATTACH") || has_word(&screened, "DETACH") || vacuum_into(&screened) {
        return Err(forbidden(
            "ATTACH, DETACH, and VACUUM INTO are not allowed in extension databases",
        ));
    }
    Ok(())
}

/// Replace string literals, quoted identifiers and comments with a space, so
/// `SELECT 'ATTACH'` is not refused.
fn strip_sql_literals(sql: &str) -> String {
    let bytes: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == '\'' || ch == '"' || ch == '`' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == ch {
                    // A doubled quote escapes itself and does NOT close the
                    // literal. Dropping this arm happens to leave the output
                    // unchanged (adjacent literals cover the same span, and
                    // every stripped region becomes a space), but any screen
                    // that looked at literal CONTENT would need it.
                    if bytes.get(i + 1) == Some(&ch) {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(' ');
            continue;
        }
        if ch == '[' {
            i = match bytes[i + 1..].iter().position(|c| *c == ']') {
                Some(offset) => i + 1 + offset + 1,
                None => bytes.len(),
            };
            out.push(' ');
            continue;
        }
        if ch == '-' && bytes.get(i + 1) == Some(&'-') {
            i = match bytes[i..].iter().position(|c| *c == '\n') {
                Some(offset) => i + offset + 1,
                None => bytes.len(),
            };
            out.push(' ');
            continue;
        }
        if ch == '/' && bytes.get(i + 1) == Some(&'*') {
            i = match find_from(&bytes, i + 2, &['*', '/']) {
                Some(end) => end + 2,
                None => bytes.len(),
            };
            out.push(' ');
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn find_from(haystack: &[char], start: usize, needle: &[char]) -> Option<usize> {
    (start..haystack.len().saturating_sub(needle.len() - 1))
        .find(|i| haystack[*i..*i + needle.len()] == *needle)
}

/// A whole-word match, so `DETACHED` is not read as `DETACH` and a column
/// called `attachment` does not fail every query that mentions it.
fn has_word(screened: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = screened[from..].find(word) {
        let at = from + offset;
        let before = screened[..at].chars().next_back();
        let after = screened[at + word.len()..].chars().next();
        let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        if boundary(before) && boundary(after) {
            return true;
        }
        from = at + word.len();
    }
    false
}

/// `VACUUM` alone is harmless; only `VACUUM ... INTO` writes a database
/// elsewhere. The `;` split keeps the two words in one statement.
fn vacuum_into(screened: &str) -> bool {
    screened
        .split(';')
        .any(|statement| has_word(statement, "VACUUM") && has_word(statement, "INTO"))
}

// --- the seams the five modules are reached through -----------------------
//
// Every trait below is DECLARED here and IMPLEMENTED elsewhere, which keeps the
// module graph acyclic and every branch here testable without a store, an
// endpoint or a model.

/// The registry, as the gate sees it. One lookup per request rather than a
/// handle, so the gate stays a pure decision and needs no database.
#[async_trait]
pub trait RegistryFacts: Send + Sync {
    async fn gate_facts(&self, extension_id: &str, tool_name: &str) -> Result<GateFacts>;
}

/// The core settings, as the broker reads them: `tts.enabled` and
/// `stt.enabled`, so a tool cannot start the speech stack the user turned off.
#[async_trait]
pub trait BrokerSettings: Send + Sync {
    async fn snapshot(&self) -> Result<SettingsRecord>;
}

/// One memory, as the list op reports it.
#[derive(Debug, Clone)]
pub struct MemoryBrief {
    pub title: String,
    pub kind: String,
    pub summary: String,
    pub updated_at_ms: i64,
}

/// The tool-facing memories API.
///
/// **Every method must treat a DISABLED memory as absent**, not merely hide it
/// from `list`: an agent that can still read it by title has been handed back
/// what the user took away. Titles rather than row ids throughout, because a
/// tool never sees an id.
#[async_trait]
pub trait ToolMemories: Send + Sync {
    /// Enabled memories only.
    async fn list(&self) -> Result<Vec<MemoryBrief>>;
    /// `not_found` for a missing OR disabled title. The STORE's title comes
    /// back, never the caller's spelling.
    async fn get(&self, title: &str) -> Result<MemoryBody>;
    /// `not_found` for a missing OR disabled title.
    async fn get_file(&self, title: &str, name: &str) -> Result<MemoryBody>;
    /// Creates when the title is new. Returns `(before, after, created)`.
    async fn write(&self, title: &str, content: &str) -> Result<MemoryWrite>;
    /// `not_found` for a missing title. Returns `(before, after)`.
    async fn edit(&self, title: &str, find: &str, replace: &str) -> Result<MemoryWrite>;
}

/// One memory's stored title and the body that was asked for.
#[derive(Debug, Clone)]
pub struct MemoryBody {
    pub title: String,
    pub content: String,
}

/// What a write or an edit changed.
#[derive(Debug, Clone)]
pub struct MemoryWrite {
    /// The store's title, not the argument's.
    pub title: String,
    pub before: String,
    pub after: String,
    pub created: bool,
}

/// One short completion on behalf of a tool.
#[derive(Debug, Clone)]
pub struct ToolCompletion {
    pub system_prompt: String,
    pub user_message: String,
    pub max_tokens: u32,
    /// Always zero, decided HERE: a tool gets one string back and cannot
    /// separate a model's `<think>` text from its answer.
    pub reasoning_budget: u32,
}

/// The tool-facing LLM.
#[async_trait]
pub trait ToolLlm: Send + Sync {
    async fn complete(&self, req: ToolCompletion) -> Result<String>;
}

/// The tool-facing speech API. The two media caps (25 MB of audio, 2,000
/// characters of text) live behind this seam, so there is one copy of each.
#[async_trait]
pub trait ToolSpeech: Send + Sync {
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>>;
    async fn transcribe(
        &self,
        audio: Vec<u8>,
        mime: &str,
        language: Option<&str>,
    ) -> Result<String>;
}

/// Bring the local model up before a completion. A no-op implementation is
/// CORRECT rather than a stub: with no in-process engine there is nothing to
/// load.
#[async_trait]
pub trait EnsureModelLoaded: Send + Sync {
    async fn ensure_loaded(&self) -> Result<()>;
}

// --- the dispatch half -----------------------------------------------------

/// Per-call output cap. Tools get short completions; long-form generation is
/// the chat path's, where the user can see it stream and stop it.
const LLM_COMPLETE_MAX_TOKENS: u32 = 1024;
const LLM_COMPLETE_DEFAULT_TOKENS: u32 = 512;

/// The user's `tts.voice` lives on the client, and the broker has no client, so
/// extension synthesis uses the schema-default voice.
const BROKER_TTS_VOICE: &str = "af_bella";

/// Open extension databases, at most this many at once, so a long-lived core
/// does not hold a handle and its WAL memory for every extension it ever ran.
const EXTENSION_DB_MAX_OPEN: usize = 16;

/// A statement slower than this is logged. A warning only: a slow query is the
/// author's problem to see, not a reason to fail their tool.
const SQL_SLOW_WARN_MS: u128 = 1_000;

/// The gate and the five modules behind it.
pub struct Broker {
    paths: Paths,
    facts: Arc<dyn RegistryFacts>,
    settings: Arc<dyn BrokerSettings>,
    memories: Arc<dyn ToolMemories>,
    llm: Arc<dyn ToolLlm>,
    speech: Arc<dyn ToolSpeech>,
    ensure_loaded: Arc<dyn EnsureModelLoaded>,
    /// Insertion order is LRU order, oldest first.
    dbs: Mutex<Vec<(String, Db)>>,
}

impl Broker {
    pub fn new(
        paths: Paths,
        facts: Arc<dyn RegistryFacts>,
        settings: Arc<dyn BrokerSettings>,
        memories: Arc<dyn ToolMemories>,
        llm: Arc<dyn ToolLlm>,
        speech: Arc<dyn ToolSpeech>,
        ensure_loaded: Arc<dyn EnsureModelLoaded>,
    ) -> Self {
        Self {
            paths,
            facts,
            settings,
            memories,
            llm,
            speech,
            ensure_loaded,
            dbs: Mutex::new(Vec::new()),
        }
    }

    /// Drop the cached handle for one extension, checkpointing its WAL.
    /// Uninstall must call this: removing the directory under an open handle
    /// leaves SQLite writing into an unlinked file.
    pub fn close_extension_db(&self, extension_id: &str) {
        let evicted = {
            let mut open = lock(&self.dbs);
            open.iter()
                .position(|(id, _)| id == extension_id)
                .map(|at| open.remove(at).1)
        };
        if let Some(db) = evicted {
            db.close();
        }
    }

    /// The extension's own `data.sqlite`, opened on first use. Eviction is
    /// safe: [`Db`] is an `Arc`, so a call already holding one keeps its
    /// connection past eviction, and the data is in the file anyway.
    fn extension_db(&self, extension_id: &str) -> Result<Db> {
        {
            let mut open = lock(&self.dbs);
            if let Some(at) = open.iter().position(|(id, _)| id == extension_id) {
                // Touch: move to the most-recently-used end.
                let entry = open.remove(at);
                let db = entry.1.clone();
                open.push(entry);
                return Ok(db);
            }
        }

        let dir = self.paths.extension_data_dir(extension_id);
        std::fs::create_dir_all(&dir)?;
        let db = Db::open(&dir.join("data.sqlite"))?;
        db.call_blocking(|conn| {
            // `busy_timeout` is deliberately shorter than the core's 5s: a
            // stuck write on a rarely-contended private database should fail
            // fast rather than hold a blocking-pool thread.
            conn.execute_batch(
                "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 1000;",
            )?;
            Ok(())
        })?;

        let evicted = {
            let mut open = lock(&self.dbs);
            // Re-check under the lock: the loser of two concurrent first uses
            // must not leave a second handle under the same key.
            if let Some(at) = open.iter().position(|(id, _)| id == extension_id) {
                let existing = open[at].1.clone();
                return Ok(existing);
            }
            open.push((extension_id.to_string(), db.clone()));
            if open.len() > EXTENSION_DB_MAX_OPEN {
                Some(open.remove(0).1)
            } else {
                None
            }
        };
        if let Some(old) = evicted {
            old.close();
        }
        Ok(db)
    }

    async fn dispatch_memories(&self, op: MemoriesOp, args: &Value) -> Result<Value> {
        Ok(match op {
            MemoriesOp::List => Value::Array(
                self.memories
                    .list()
                    .await?
                    .into_iter()
                    .map(|m| {
                        serde_json::json!({
                            "title": m.title,
                            "kind": m.kind,
                            "summary": m.summary,
                            "updatedAtMs": m.updated_at_ms,
                        })
                    })
                    .collect(),
            ),
            MemoriesOp::Get => {
                let body = self
                    .memories
                    .get(&arg_string(args, "title", false)?)
                    .await?;
                serde_json::json!({ "title": body.title, "content": body.content })
            }
            MemoriesOp::GetFile => {
                let title = arg_string(args, "title", false)?;
                let name = arg_string(args, "name", false)?;
                let body = self.memories.get_file(&title, &name).await?;
                // `name` is the argument's: only the title is resolved.
                serde_json::json!({ "title": body.title, "name": name, "content": body.content })
            }
            MemoriesOp::Write => {
                let title = arg_string(args, "title", false)?;
                let content = arg_string(args, "content", true)?;
                let out = self.memories.write(&title, &content).await?;
                serde_json::json!({
                    "title": out.title,
                    "before": out.before,
                    "after": out.after,
                    "created": out.created,
                })
            }
            MemoriesOp::Edit => {
                let title = arg_string(args, "title", false)?;
                let find = arg_string(args, "find", false)?;
                let replace = arg_string(args, "replace", true)?;
                let out = self.memories.edit(&title, &find, &replace).await?;
                // No `created`: an edit cannot create.
                serde_json::json!({
                    "title": out.title,
                    "before": out.before,
                    "after": out.after,
                })
            }
        })
    }

    async fn dispatch_llm(&self, op: &str, args: &Value) -> Result<Value> {
        if op != "complete" {
            return Err(validation(format!("unknown llm op \"{op}\"")));
        }
        let prompt = arg_string(args, "prompt", false)?;
        let system_prompt = args
            .get("systemPrompt")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let max_tokens = args
            .get("maxTokens")
            .and_then(Value::as_f64)
            .filter(|n| *n > 0.0)
            .map_or(LLM_COMPLETE_DEFAULT_TOKENS, |n| {
                n.floor().min(f64::from(LLM_COMPLETE_MAX_TOKENS)) as u32
            })
            .min(LLM_COMPLETE_MAX_TOKENS);

        // Before the endpoint is resolved: a cold local model would otherwise
        // fail the completion on a connection refused.
        self.ensure_loaded.ensure_loaded().await?;
        let text = self
            .llm
            .complete(ToolCompletion {
                system_prompt,
                user_message: prompt,
                max_tokens,
                reasoning_budget: 0,
            })
            .await?;
        Ok(serde_json::json!({ "text": text }))
    }

    async fn dispatch_tts(&self, op: &str, args: &Value) -> Result<Value> {
        if op != "speak" {
            return Err(validation(format!("unknown tts op \"{op}\"")));
        }
        let settings = self.settings.snapshot().await?;
        if !bool_setting(&settings, "tts.enabled", false) {
            return Err(AppError::new(
                ErrorCode::ServerUnavailable,
                "Text-to-Speech is turned off in settings",
            ));
        }
        let text = arg_string(args, "text", false)?;
        // The length cap is the ToolSpeech implementation's, kept in one place.
        let wav = self.speech.synthesize(&text, BROKER_TTS_VOICE).await?;
        Ok(serde_json::json!({
            "dataB64": BASE64_STANDARD.encode(&wav),
            "mime": "audio/wav",
            "sampleRate": wav_sample_rate(&wav),
        }))
    }

    async fn dispatch_stt(&self, op: &str, args: &Value) -> Result<Value> {
        if op != "transcribe" {
            return Err(validation(format!("unknown stt op \"{op}\"")));
        }
        let settings = self.settings.snapshot().await?;
        if !bool_setting(&settings, "stt.enabled", false) {
            return Err(AppError::new(
                ErrorCode::ServerUnavailable,
                "Speech-to-Text is turned off in settings",
            ));
        }
        let data_b64 = arg_string(args, "dataB64", false)?;
        let bytes = BASE64_STANDARD
            .decode(data_b64.as_bytes())
            .map_err(|_| validation("stt dataB64 is not valid base64"))?;
        let mime = args
            .get("mime")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .unwrap_or("audio/wav")
            .to_string();
        let language = args
            .get("language")
            .and_then(Value::as_str)
            .filter(|l| !l.is_empty())
            .map(str::to_string);
        // The 25 MB cap is the implementation's; unreachable here anyway, the
        // worker reader drops an NDJSON frame over 16 MB before parsing it.
        let text = self
            .speech
            .transcribe(bytes, &mime, language.as_deref())
            .await?;
        Ok(serde_json::json!({ "text": text }))
    }

    async fn dispatch_db(
        &self,
        extension_id: &str,
        tool_name: &str,
        op: &str,
        args: &Value,
    ) -> Result<Value> {
        let sql = arg_string(args, "sql", false)?;
        let params = db_params(args)?;
        assert_sql_allowed(&sql).inspect_err(|_| {
            tracing::warn!(
                target: "loca::modulebroker",
                "blocked db statement from {extension_id}/{tool_name}: {}",
                sql_snippet(&sql),
            );
        })?;
        let db = self.extension_db(extension_id)?;

        let started = std::time::Instant::now();
        let query = op == "query";
        let execute = op == "execute";
        if !query && !execute {
            return Err(validation(format!("unknown db op \"{op}\"")));
        }
        let statement = sql.clone();
        let outcome = db
            .call(move |conn| {
                if query {
                    run_query(conn, &statement, &params)
                } else {
                    run_execute(conn, &statement, &params)
                }
            })
            .await;

        let elapsed = started.elapsed().as_millis();
        if elapsed >= SQL_SLOW_WARN_MS {
            tracing::warn!(
                target: "loca::modulebroker",
                "slow db {op} ({elapsed}ms) from {extension_id}/{tool_name}: {}",
                sql_snippet(&sql),
            );
        }
        outcome.inspect_err(|err| {
            tracing::warn!(
                target: "loca::modulebroker",
                "db {op} from {extension_id}/{tool_name} failed: {}; sql: {}",
                err.message, sql_snippet(&sql),
            );
        })
    }
}

#[async_trait]
impl ModuleDispatch for Broker {
    async fn dispatch(&self, call: ModuleCall, prompter: Arc<dyn ModulePrompter>) -> Result<Value> {
        let facts = self
            .facts
            .gate_facts(&call.extension_id, &call.tool_name)
            .await?;
        gate(call.module, &call.op, &facts, prompter.as_ref()).await?;

        match call.module {
            ModuleName::Memories => {
                // Already refused by the gate; this parse only makes the match
                // exhaustive.
                let op = MemoriesOp::of(&call.op)
                    .ok_or_else(|| validation(format!("unknown memories op \"{}\"", call.op)))?;
                self.dispatch_memories(op, &call.args).await
            }
            ModuleName::Db => {
                self.dispatch_db(&call.extension_id, &call.tool_name, &call.op, &call.args)
                    .await
            }
            ModuleName::Llm => self.dispatch_llm(&call.op, &call.args).await,
            ModuleName::Tts => self.dispatch_tts(&call.op, &call.args).await,
            ModuleName::Stt => self.dispatch_stt(&call.op, &call.args).await,
        }
    }
}

/// A required string argument. `allow_empty` is false for anything that names
/// something: an empty name is a missing field.
fn arg_string(args: &Value, key: &str, allow_empty: bool) -> Result<String> {
    match args.get(key).and_then(Value::as_str) {
        Some(value) if allow_empty || !value.is_empty() => Ok(value.to_string()),
        _ => Err(validation(format!(
            "module op requires string argument \"{key}\""
        ))),
    }
}

/// One bind parameter, restricted to what SQLite can take from JSON. An object
/// or array is refused rather than stringified into a silent corruption.
#[derive(Debug, Clone)]
enum BindValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
}

impl rusqlite::ToSql for BindValue {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, ValueRef};
        Ok(match self {
            Self::Null => ToSqlOutput::Borrowed(ValueRef::Null),
            Self::Integer(i) => ToSqlOutput::Borrowed(ValueRef::Integer(*i)),
            Self::Real(f) => ToSqlOutput::Borrowed(ValueRef::Real(*f)),
            Self::Text(s) => ToSqlOutput::Borrowed(ValueRef::Text(s.as_bytes())),
        })
    }
}

fn db_params(args: &Value) -> Result<Vec<BindValue>> {
    let params = match args.get("params") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(validation("db op \"params\" must be an array")),
    };
    params
        .iter()
        .map(|param| match param {
            Value::Null => Ok(BindValue::Null),
            // A JSON `true` binds as SQLite's 1.
            Value::Bool(b) => Ok(BindValue::Integer(i64::from(*b))),
            Value::Number(n) => Ok(n
                .as_i64()
                .map(BindValue::Integer)
                .unwrap_or_else(|| BindValue::Real(n.as_f64().unwrap_or(f64::NAN)))),
            Value::String(s) => Ok(BindValue::Text(s.clone())),
            _ => Err(validation(
                "db params must be strings, numbers, booleans, or null",
            )),
        })
        .collect()
}

fn run_query(conn: &mut rusqlite::Connection, sql: &str, params: &[BindValue]) -> Result<Value> {
    let mut stmt = conn.prepare(sql)?;
    let names: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    let mut out: Vec<Value> = Vec::new();
    while let Some(row) = rows.next()? {
        // Bail at the cap rather than materialising every row and counting, so
        // a runaway query costs no unbounded allocation.
        if out.len() >= SQL_QUERY_MAX_ROWS {
            return Err(validation(format!(
                "query returned more than {SQL_QUERY_MAX_ROWS} rows; narrow it with LIMIT"
            )));
        }
        let mut obj = Map::new();
        for (index, name) in names.iter().enumerate() {
            obj.insert(name.clone(), sql_value_to_json(row.get_ref(index)?));
        }
        out.push(Value::Object(obj));
    }
    Ok(Value::Array(out))
}

fn run_execute(conn: &mut rusqlite::Connection, sql: &str, params: &[BindValue]) -> Result<Value> {
    let changes = conn.execute(sql, rusqlite::params_from_iter(params.iter()))?;
    let rowid = conn.last_insert_rowid();
    Ok(serde_json::json!({
        "changes": changes,
        "lastInsertRowId": json_safe_integer(rowid),
    }))
}

/// One column value, on a JSON wire. An INTEGER past 2^53 has no JSON number
/// that round-trips through the worker's `JSON.parse`, so it becomes a decimal
/// string; a non-finite REAL has no JSON literal, so it becomes null.
fn sql_value_to_json(value: rusqlite::types::ValueRef<'_>) -> Value {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json_safe_integer(i),
        ValueRef::Real(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
        ValueRef::Text(bytes) => Value::String(String::from_utf8_lossy(bytes).into_owned()),
        // Byte-keyed, not base64: `JSON.stringify(Uint8Array)`'s shape, which
        // is what an extension reading a BLOB column already handles.
        ValueRef::Blob(bytes) => Value::Object(
            bytes
                .iter()
                .enumerate()
                .map(|(index, byte)| (index.to_string(), Value::from(*byte)))
                .collect(),
        ),
    }
}

/// JavaScript's `Number.MAX_SAFE_INTEGER`. The bound is the worker's, not
/// SQLite's: the tool parses this value with `JSON.parse`.
const JS_MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

fn json_safe_integer(value: i64) -> Value {
    if value.abs() <= JS_MAX_SAFE_INTEGER {
        Value::from(value)
    } else {
        Value::String(value.to_string())
    }
}

fn sql_snippet(sql: &str) -> String {
    match sql.char_indices().nth(200) {
        Some((at, _)) => format!("{}…", &sql[..at]),
        None => sql.to_string(),
    }
}

/// The WAV sample rate: a little-endian `u32` at byte offset 24. A clip too
/// short reports the local engine's 24 kHz, because zero is divided by.
fn wav_sample_rate(wav: &[u8]) -> u32 {
    match wav.get(24..28) {
        Some(bytes) => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        None => 24_000,
    }
}

fn validation(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::ValidationError, message)
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use async_trait::async_trait;

    /// Records what it was asked and answers a scripted verdict.
    struct Scripted {
        allow: bool,
        asked: Mutex<Vec<ModulePrompt>>,
    }

    impl Scripted {
        fn new(allow: bool) -> Self {
            Self {
                allow,
                asked: Mutex::new(Vec::new()),
            }
        }
        fn asked(&self) -> Vec<ModulePrompt> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ModulePrompter for Scripted {
        async fn ask(&self, prompt: ModulePrompt) -> bool {
            self.asked.lock().unwrap().push(prompt);
            self.allow
        }
    }

    fn memories_decl(access: PermissionDeclAccess) -> PermissionDecl {
        PermissionDecl::Memories {
            access,
            optional: None,
            reason: "to keep your notes".to_string(),
        }
    }

    fn llm_decl() -> PermissionDecl {
        PermissionDecl::Llm {
            optional: None,
            reason: "to summarise".to_string(),
        }
    }

    fn grant(decl: &PermissionDecl, state: GrantState) -> Grant {
        Grant {
            granted_at_ms: 0,
            permission_key: permission_key(decl),
            permission_kind: permission_kind_of(decl),
            state,
            tool_id: "t1".to_string(),
        }
    }

    fn facts(required: Vec<PermissionDecl>, grants: Vec<Grant>) -> GateFacts {
        GateFacts {
            has_database: false,
            undeclared_policy: UndeclaredPolicy::Deny,
            required,
            grants,
        }
    }

    // --- db, which is declaration gated and nothing else --------------------

    #[tokio::test]
    async fn db_is_refused_unless_the_manifest_declared_it() {
        let prompter = Scripted::new(true);
        let mut f = facts(vec![], vec![]);
        let err = gate(ModuleName::Db, "query", &f, &prompter)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
        // `database` is reviewed at install time, so it is never a prompt.
        assert!(
            prompter.asked().is_empty(),
            "an undeclared database was offered to the user as a prompt"
        );

        f.has_database = true;
        assert!(gate(ModuleName::Db, "query", &f, &prompter).await.is_ok());
        assert!(prompter.asked().is_empty(), "a declared database prompted");
    }

    // --- the grant-gated four -----------------------------------------------

    #[tokio::test]
    async fn a_granted_declaration_dispatches_without_asking() {
        let decl = llm_decl();
        let f = facts(vec![decl.clone()], vec![grant(&decl, GrantState::Granted)]);
        let prompter = Scripted::new(false);
        assert!(gate(ModuleName::Llm, "complete", &f, &prompter)
            .await
            .is_ok());
        assert!(prompter.asked().is_empty(), "a granted permission prompted");
    }

    #[tokio::test]
    async fn a_denied_grant_refuses_without_asking() {
        let decl = llm_decl();
        let f = facts(vec![decl.clone()], vec![grant(&decl, GrantState::Denied)]);
        let prompter = Scripted::new(true);
        let err = gate(ModuleName::Llm, "complete", &f, &prompter)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
        assert!(
            prompter.asked().is_empty(),
            "a denied permission was re-offered to the user"
        );
    }

    #[tokio::test]
    async fn a_declaration_with_no_grant_row_asks_rather_than_assuming() {
        let decl = llm_decl();
        let f = facts(vec![decl.clone()], vec![]);
        let prompter = Scripted::new(true);
        assert!(gate(ModuleName::Llm, "complete", &f, &prompter)
            .await
            .is_ok());
        let asked = prompter.asked();
        assert_eq!(asked.len(), 1);
        assert!(
            asked[0].declared,
            "a declared permission was shown as undeclared"
        );
        assert_eq!(asked[0].reason.as_deref(), Some("to summarise"));

        let prompter = Scripted::new(false);
        assert_eq!(
            gate(ModuleName::Llm, "complete", &f, &prompter)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Forbidden
        );
    }

    #[tokio::test]
    async fn an_ask_state_grant_asks_every_time_rather_than_being_remembered() {
        // The approval is call scoped: "always allow" lives in the detail view.
        let decl = llm_decl();
        let f = facts(vec![decl.clone()], vec![grant(&decl, GrantState::Ask)]);
        let prompter = Scripted::new(true);
        for _ in 0..3 {
            assert!(gate(ModuleName::Llm, "complete", &f, &prompter)
                .await
                .is_ok());
        }
        assert_eq!(
            prompter.asked().len(),
            3,
            "an approval was remembered across calls"
        );
        assert_eq!(
            f.grants[0].state,
            GrantState::Ask,
            "the gate wrote the grant row"
        );
    }

    // --- the undeclared policy ----------------------------------------------

    #[tokio::test]
    async fn an_undeclared_access_follows_the_extensions_policy() {
        let prompter = Scripted::new(true);
        let mut f = facts(vec![], vec![]);

        let err = gate(ModuleName::Tts, "speak", &f, &prompter)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
        assert!(
            prompter.asked().is_empty(),
            "a deny policy still asked the user"
        );

        f.undeclared_policy = UndeclaredPolicy::Ask;
        assert!(gate(ModuleName::Tts, "speak", &f, &prompter).await.is_ok());
        let asked = prompter.asked();
        assert_eq!(asked.len(), 1);
        assert!(
            !asked[0].declared,
            "an undeclared access was shown to the user as declared"
        );
        assert!(
            asked[0].reason.is_none(),
            "an undeclared access invented a reason the author never wrote"
        );
    }

    #[tokio::test]
    async fn a_declaration_of_another_kind_does_not_cover_this_one() {
        let decl = PermissionDecl::Tts {
            optional: None,
            reason: "to speak".to_string(),
        };
        let f = facts(vec![decl.clone()], vec![grant(&decl, GrantState::Granted)]);
        let prompter = Scripted::new(false);
        assert!(gate(ModuleName::Stt, "transcribe", &f, &prompter)
            .await
            .is_err());
        assert!(gate(ModuleName::Tts, "speak", &f, &prompter).await.is_ok());
    }

    // --- memories, the one module with two accesses -------------------------

    #[tokio::test]
    async fn a_write_declaration_covers_a_read_but_not_the_other_way_round() {
        let write = memories_decl(PermissionDeclAccess::Write);
        let f = facts(
            vec![write.clone()],
            vec![grant(&write, GrantState::Granted)],
        );
        let prompter = Scripted::new(false);
        for op in ["list", "get", "getFile", "write", "edit"] {
            assert!(
                gate(ModuleName::Memories, op, &f, &prompter).await.is_ok(),
                "a write grant did not cover {op}"
            );
        }

        let read = memories_decl(PermissionDeclAccess::Read);
        let f = facts(vec![read.clone()], vec![grant(&read, GrantState::Granted)]);
        for op in ["list", "get", "getFile"] {
            assert!(
                gate(ModuleName::Memories, op, &f, &prompter).await.is_ok(),
                "a read grant did not cover {op}"
            );
        }
        for op in ["write", "edit"] {
            assert!(
                gate(ModuleName::Memories, op, &f, &prompter).await.is_err(),
                "a read grant let {op} through"
            );
        }
    }

    #[tokio::test]
    async fn the_prompt_names_which_half_of_memories_is_being_asked_for() {
        let f = facts(vec![], vec![]);
        let f = GateFacts {
            undeclared_policy: UndeclaredPolicy::Ask,
            ..f
        };
        let prompter = Scripted::new(true);
        gate(ModuleName::Memories, "get", &f, &prompter)
            .await
            .unwrap();
        gate(ModuleName::Memories, "edit", &f, &prompter)
            .await
            .unwrap();
        let asked = prompter.asked();
        assert_eq!(asked[0].resource, "read");
        assert_eq!(asked[1].resource, "write");
    }

    #[tokio::test]
    async fn an_unknown_memories_op_is_refused_before_it_can_be_gated_at_all() {
        let write = memories_decl(PermissionDeclAccess::Write);
        let f = facts(
            vec![write.clone()],
            vec![grant(&write, GrantState::Granted)],
        );
        let prompter = Scripted::new(true);
        let err = gate(ModuleName::Memories, "purge", &f, &prompter)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(
            prompter.asked().is_empty(),
            "an unknown op was offered to the user as a decision"
        );
    }

    // --- the SQL containment screen -----------------------------------------

    #[test]
    fn the_three_statements_that_reach_another_database_file_are_refused() {
        for sql in [
            "ATTACH DATABASE '/tmp/x.db' AS x",
            "attach database '/tmp/x.db' as x",
            "SELECT 1; ATTACH DATABASE '/tmp/x.db' AS x",
            "DETACH DATABASE x",
            "VACUUM INTO '/tmp/copy.db'",
            "VACUUM main INTO '/tmp/copy.db'",
            "/* harmless */ ATTACH DATABASE '/tmp/x.db' AS x",
            "-- harmless\nATTACH DATABASE '/tmp/x.db' AS x",
        ] {
            let err = assert_sql_allowed(sql).unwrap_err();
            assert_eq!(err.code, ErrorCode::Forbidden, "allowed: {sql}");
        }
    }

    #[test]
    fn the_words_are_only_refused_as_statements_never_as_data() {
        for sql in [
            "SELECT 'ATTACH'",
            "SELECT 'attach' AS word",
            "INSERT INTO notes (body) VALUES ('please DETACH the form')",
            "SELECT \"ATTACH\" FROM t",
            "SELECT `VACUUM INTO` FROM t",
            "SELECT [VACUUM INTO] FROM t",
            "-- ATTACH DATABASE\nSELECT 1",
            "/* ATTACH DATABASE */ SELECT 1",
            "SELECT 'it''s ATTACH time'",
            "VACUUM",
            "SELECT * FROM attachments",
            "SELECT detached FROM t",
            "SELECT into_field FROM t",
        ] {
            assert!(assert_sql_allowed(sql).is_ok(), "refused: {sql}");
        }
    }

    #[test]
    fn a_literal_containing_a_doubled_quote_is_erased_whole() {
        // Pins the OUTPUT, not the escape rule: the whole literal goes,
        // including the `b` after the doubled quote.
        let stripped = strip_sql_literals("SELECT 'a''b' , 1");
        assert!(!stripped.contains('b'), "literal survived: {stripped:?}");
        assert!(stripped.contains("SELECT") && stripped.contains(", 1"));
        assert!(assert_sql_allowed("SELECT 'a''ATTACH''b'").is_ok());
        // The reverse, so a stripper eating the whole remainder still fails.
        assert!(assert_sql_allowed("SELECT 'a''b'; ATTACH DATABASE '/x' AS x").is_err());
    }

    #[test]
    fn an_unterminated_literal_or_comment_does_not_read_past_the_end() {
        // Malformed SQL is SQLite's to reject; this only must not run off the
        // end.
        for sql in [
            "SELECT 'unclosed",
            "SELECT /* unclosed",
            "SELECT [unclosed",
            "SELECT `x",
        ] {
            assert!(assert_sql_allowed(sql).is_ok(), "unexpected refusal: {sql}");
        }
    }

    #[test]
    fn vacuum_and_into_must_be_the_same_statement() {
        assert!(assert_sql_allowed("VACUUM; SELECT 1 INTO x").is_ok());
        assert!(assert_sql_allowed("VACUUM INTO '/tmp/c.db'").is_err());
    }

    #[test]
    fn the_row_cap_is_ten_thousand() {
        assert_eq!(SQL_QUERY_MAX_ROWS, 10_000);
    }

    // --- the dispatch half ---------------------------------------------------

    /// Answers the gate with whatever the test staged, for every tool.
    struct FixedFacts(GateFacts);

    #[async_trait]
    impl RegistryFacts for FixedFacts {
        async fn gate_facts(&self, _extension_id: &str, _tool_name: &str) -> Result<GateFacts> {
            Ok(self.0.clone())
        }
    }

    struct FixedSettings(SettingsRecord);

    #[async_trait]
    impl BrokerSettings for FixedSettings {
        async fn snapshot(&self) -> Result<SettingsRecord> {
            Ok(self.0.clone())
        }
    }

    /// Every module fake writes here, so a test can assert on ORDER across
    /// subsystems.
    #[derive(Default)]
    struct Trace(Mutex<Vec<String>>);

    impl Trace {
        fn push(&self, entry: impl Into<String>) {
            lock(&self.0).push(entry.into());
        }
        fn entries(&self) -> Vec<String> {
            lock(&self.0).clone()
        }
    }

    struct FakeMemories(Arc<Trace>);

    #[async_trait]
    impl ToolMemories for FakeMemories {
        async fn list(&self) -> Result<Vec<MemoryBrief>> {
            self.0.push("memories.list");
            Ok(vec![MemoryBrief {
                title: "Kitchen".to_string(),
                kind: "knowledge".to_string(),
                summary: "the good knives".to_string(),
                updated_at_ms: 1_700_000_000_000,
            }])
        }
        async fn get(&self, title: &str) -> Result<MemoryBody> {
            self.0.push(format!("memories.get({title})"));
            Ok(MemoryBody {
                title: format!("stored:{title}"),
                content: "body".to_string(),
            })
        }
        async fn get_file(&self, title: &str, name: &str) -> Result<MemoryBody> {
            self.0.push(format!("memories.getFile({title},{name})"));
            Ok(MemoryBody {
                title: format!("stored:{title}"),
                content: "file body".to_string(),
            })
        }
        async fn write(&self, title: &str, content: &str) -> Result<MemoryWrite> {
            self.0.push(format!("memories.write({title},{content})"));
            Ok(MemoryWrite {
                title: format!("stored:{title}"),
                before: "old".to_string(),
                after: content.to_string(),
                created: true,
            })
        }
        async fn edit(&self, title: &str, find: &str, replace: &str) -> Result<MemoryWrite> {
            self.0
                .push(format!("memories.edit({title},{find},{replace})"));
            Ok(MemoryWrite {
                title: format!("stored:{title}"),
                before: "old".to_string(),
                after: "new".to_string(),
                created: false,
            })
        }
    }

    struct FakeLlm {
        trace: Arc<Trace>,
        seen: Mutex<Vec<ToolCompletion>>,
    }

    #[async_trait]
    impl ToolLlm for FakeLlm {
        async fn complete(&self, req: ToolCompletion) -> Result<String> {
            self.trace.push("llm.complete");
            lock(&self.seen).push(req);
            Ok("answer".to_string())
        }
    }

    /// What one `transcribe` call was handed: audio, mime, language hint.
    type Transcription = (Vec<u8>, String, Option<String>);

    struct FakeSpeech {
        trace: Arc<Trace>,
        wav: Vec<u8>,
        seen_voice: Mutex<Vec<String>>,
        seen_transcribe: Mutex<Vec<Transcription>>,
    }

    #[async_trait]
    impl ToolSpeech for FakeSpeech {
        async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
            self.trace.push(format!("speech.synthesize({text})"));
            lock(&self.seen_voice).push(voice.to_string());
            Ok(self.wav.clone())
        }
        async fn transcribe(
            &self,
            audio: Vec<u8>,
            mime: &str,
            language: Option<&str>,
        ) -> Result<String> {
            self.trace.push("speech.transcribe");
            lock(&self.seen_transcribe).push((
                audio,
                mime.to_string(),
                language.map(str::to_string),
            ));
            Ok("transcript".to_string())
        }
    }

    struct FakeEnsure(Arc<Trace>);

    #[async_trait]
    impl EnsureModelLoaded for FakeEnsure {
        async fn ensure_loaded(&self) -> Result<()> {
            self.0.push("ensure_loaded");
            Ok(())
        }
    }

    struct Harness {
        broker: Broker,
        trace: Arc<Trace>,
        llm: Arc<FakeLlm>,
        speech: Arc<FakeSpeech>,
        prompter: Arc<Scripted>,
        _dir: tempfile::TempDir,
    }

    impl Harness {
        fn new(facts: GateFacts, settings: &[(&str, Value)]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let trace = Arc::new(Trace::default());
            let llm = Arc::new(FakeLlm {
                trace: Arc::clone(&trace),
                seen: Mutex::new(Vec::new()),
            });
            let speech = Arc::new(FakeSpeech {
                trace: Arc::clone(&trace),
                wav: wav_bytes(44_100),
                seen_voice: Mutex::new(Vec::new()),
                seen_transcribe: Mutex::new(Vec::new()),
            });
            let record: SettingsRecord = settings
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect();
            Self {
                broker: Broker::new(
                    Paths::rooted(dir.path()),
                    Arc::new(FixedFacts(facts)),
                    Arc::new(FixedSettings(record)),
                    Arc::new(FakeMemories(Arc::clone(&trace))),
                    Arc::clone(&llm) as Arc<dyn ToolLlm>,
                    Arc::clone(&speech) as Arc<dyn ToolSpeech>,
                    Arc::new(FakeEnsure(Arc::clone(&trace))),
                ),
                trace,
                llm,
                speech,
                prompter: Arc::new(Scripted::new(true)),
                _dir: dir,
            }
        }

        async fn call(&self, module: ModuleName, op: &str, args: Value) -> Result<Value> {
            self.broker
                .dispatch(
                    ModuleCall {
                        extension_id: "ext".to_string(),
                        tool_name: "t1".to_string(),
                        call_id: "c1".to_string(),
                        module,
                        op: op.to_string(),
                        args,
                    },
                    Arc::clone(&self.prompter) as Arc<dyn ModulePrompter>,
                )
                .await
        }

        async fn mem(&self, op: &str, args: Value) -> Result<Value> {
            self.call(ModuleName::Memories, op, args).await
        }

        async fn execute(&self, sql: &str) -> Result<Value> {
            self.call(ModuleName::Db, "execute", serde_json::json!({ "sql": sql }))
                .await
        }

        async fn query(&self, sql: &str) -> Result<Value> {
            self.call(ModuleName::Db, "query", serde_json::json!({ "sql": sql }))
                .await
        }
    }

    /// A minimal WAV header with the sample rate at byte 24, the only field
    /// read.
    fn wav_bytes(rate: u32) -> Vec<u8> {
        let mut wav = vec![0u8; 44];
        wav[..4].copy_from_slice(b"RIFF");
        wav[8..12].copy_from_slice(b"WAVE");
        wav[24..28].copy_from_slice(&rate.to_le_bytes());
        wav
    }

    /// Everything declared and granted, so a dispatch test is not accidentally
    /// a gate test.
    fn open_facts() -> GateFacts {
        let decls = vec![
            memories_decl(PermissionDeclAccess::Write),
            llm_decl(),
            PermissionDecl::Tts {
                optional: None,
                reason: "to speak".to_string(),
            },
            PermissionDecl::Stt {
                optional: None,
                reason: "to listen".to_string(),
            },
        ];
        let grants = decls
            .iter()
            .map(|decl| grant(decl, GrantState::Granted))
            .collect();
        GateFacts {
            has_database: true,
            undeclared_policy: UndeclaredPolicy::Deny,
            required: decls,
            grants,
        }
    }

    fn open() -> Harness {
        Harness::new(
            open_facts(),
            &[
                ("tts.enabled", Value::Bool(true)),
                ("stt.enabled", Value::Bool(true)),
            ],
        )
    }

    #[tokio::test]
    async fn the_gate_runs_before_the_module_is_reached_at_all() {
        // A refusal must happen BEFORE the side effect; every error-code
        // assertion in this file passes either way.
        let mut facts = open_facts();
        facts.grants = vec![grant(
            &memories_decl(PermissionDeclAccess::Write),
            GrantState::Denied,
        )];
        let h = Harness::new(facts, &[]);
        let err = h
            .mem("write", serde_json::json!({ "title": "T", "content": "C" }))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
        assert!(
            h.trace.entries().is_empty(),
            "the module ran despite the refusal: {:?}",
            h.trace.entries()
        );
    }

    #[tokio::test]
    async fn every_memories_op_answers_in_the_shape_the_tool_expects() {
        let h = open();

        let list = h.mem("list", Value::Null).await.unwrap();
        assert_eq!(
            list,
            serde_json::json!([{
                "title": "Kitchen",
                "kind": "knowledge",
                "summary": "the good knives",
                "updatedAtMs": 1_700_000_000_000i64,
            }])
        );

        let got = h
            .mem("get", serde_json::json!({ "title": "Kitchen" }))
            .await
            .unwrap();
        // The STORED title, so a tool can tell a near-miss from an exact match.
        assert_eq!(
            got,
            serde_json::json!({ "title": "stored:Kitchen", "content": "body" })
        );

        let file = h
            .mem(
                "getFile",
                serde_json::json!({ "title": "Kitchen", "name": "knives.md" }),
            )
            .await
            .unwrap();
        assert_eq!(
            file,
            serde_json::json!({
                "title": "stored:Kitchen",
                // The filename IS the argument's.
                "name": "knives.md",
                "content": "file body",
            })
        );

        let written = h
            .mem(
                "write",
                serde_json::json!({ "title": "Kitchen", "content": "" }),
            )
            .await
            .unwrap();
        assert_eq!(
            written,
            serde_json::json!({
                "title": "stored:Kitchen",
                "before": "old",
                "after": "",
                "created": true,
            }),
            "an empty content must be a write, not a missing argument"
        );

        let edited = h
            .mem(
                "edit",
                serde_json::json!({ "title": "Kitchen", "find": "a", "replace": "" }),
            )
            .await
            .unwrap();
        // No `created` key at all: an edit cannot create.
        assert_eq!(
            edited,
            serde_json::json!({ "title": "stored:Kitchen", "before": "old", "after": "new" })
        );
    }

    #[tokio::test]
    async fn a_memories_op_missing_its_name_is_refused_rather_than_run_on_an_empty_title() {
        let h = open();
        for (op, args) in [
            ("get", serde_json::json!({ "title": "" })),
            ("get", serde_json::json!({})),
            ("getFile", serde_json::json!({ "title": "T" })),
            ("write", serde_json::json!({ "title": "T" })),
            ("edit", serde_json::json!({ "title": "T", "find": "" })),
        ] {
            let err = h.mem(op, args.clone()).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::ValidationError, "{op} {args}");
        }
        assert!(
            h.trace.entries().is_empty(),
            "a malformed request still reached the store"
        );
    }

    #[tokio::test]
    async fn the_completion_budget_is_defaulted_capped_and_floored() {
        let h = open();
        for (asked, expected) in [
            (Value::Null, 512u32),
            (serde_json::json!(0), 512),
            (serde_json::json!(-5), 512),
            (serde_json::json!(64), 64),
            (serde_json::json!(10.7), 10),
            (serde_json::json!(4096), 1024),
        ] {
            let mut args = serde_json::json!({ "prompt": "hi" });
            if !asked.is_null() {
                args["maxTokens"] = asked.clone();
            }
            h.call(ModuleName::Llm, "complete", args).await.unwrap();
            let seen = lock(&h.llm.seen);
            let last = seen.last().unwrap();
            assert_eq!(last.max_tokens, expected, "maxTokens {asked}");
        }
    }

    #[tokio::test]
    async fn a_tool_completion_is_never_given_a_thinking_budget() {
        let h = open();
        let out = h
            .call(
                ModuleName::Llm,
                "complete",
                serde_json::json!({ "prompt": "hi", "systemPrompt": "be brief" }),
            )
            .await
            .unwrap();
        assert_eq!(out, serde_json::json!({ "text": "answer" }));
        let seen = lock(&h.llm.seen);
        assert_eq!(seen[0].reasoning_budget, 0);
        assert_eq!(seen[0].system_prompt, "be brief");
        assert_eq!(seen[0].user_message, "hi");
    }

    #[tokio::test]
    async fn the_model_is_brought_up_before_the_completion_goes_out() {
        // The other order fails only against a cold local model, which a fake
        // endpoint never reaches.
        let h = open();
        h.call(
            ModuleName::Llm,
            "complete",
            serde_json::json!({ "prompt": "hi" }),
        )
        .await
        .unwrap();
        assert_eq!(h.trace.entries(), vec!["ensure_loaded", "llm.complete"]);
    }

    #[tokio::test]
    async fn an_unknown_op_is_refused_by_every_all_or_nothing_module() {
        let h = open();
        for (module, op) in [
            (ModuleName::Llm, "stream"),
            (ModuleName::Tts, "sing"),
            (ModuleName::Stt, "listen"),
            (ModuleName::Db, "drop"),
        ] {
            let err = h
                .call(module, op, serde_json::json!({ "sql": "SELECT 1" }))
                .await
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::ValidationError, "{module:?} {op}");
        }
        assert!(
            !h.trace.entries().contains(&"llm.complete".to_string()),
            "an unknown op reached its module anyway"
        );
    }

    #[tokio::test]
    async fn speech_a_tool_asks_for_is_refused_while_the_user_has_it_switched_off() {
        // Both switches default OFF when the key is absent, as on a fresh
        // install.
        let off = Harness::new(open_facts(), &[]);
        for (module, op, args) in [
            (
                ModuleName::Tts,
                "speak",
                serde_json::json!({ "text": "hi" }),
            ),
            (
                ModuleName::Stt,
                "transcribe",
                serde_json::json!({ "dataB64": "AAAA" }),
            ),
        ] {
            let err = off.call(module, op, args).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::ServerUnavailable, "{module:?}");
        }
        assert!(
            off.trace.entries().is_empty(),
            "the speech engine was reached with the switch off: {:?}",
            off.trace.entries()
        );

        // And an explicit `false` is not merely a missing key.
        let explicit = Harness::new(
            open_facts(),
            &[
                ("tts.enabled", Value::Bool(false)),
                ("stt.enabled", Value::Bool(false)),
            ],
        );
        assert_eq!(
            explicit
                .call(
                    ModuleName::Tts,
                    "speak",
                    serde_json::json!({ "text": "hi" })
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::ServerUnavailable
        );
    }

    #[tokio::test]
    async fn synthesis_answers_with_the_clips_own_sample_rate() {
        let h = open();
        let out = h
            .call(
                ModuleName::Tts,
                "speak",
                serde_json::json!({ "text": "hi" }),
            )
            .await
            .unwrap();
        assert_eq!(out["mime"], "audio/wav");
        assert_eq!(out["sampleRate"], 44_100);
        assert_eq!(
            out["dataB64"].as_str().unwrap(),
            BASE64_STANDARD.encode(wav_bytes(44_100)),
        );
        // The schema default: the user's `tts.voice` lives on the client.
        assert_eq!(lock(&h.speech.seen_voice)[0], "af_bella");
    }

    #[test]
    fn a_clip_too_short_to_carry_a_header_reports_the_local_rate() {
        assert_eq!(wav_sample_rate(&[]), 24_000);
        assert_eq!(wav_sample_rate(&[0u8; 27]), 24_000);
        assert_eq!(wav_sample_rate(&wav_bytes(16_000)), 16_000);
    }

    #[tokio::test]
    async fn transcription_decodes_the_audio_and_defaults_only_what_is_absent() {
        let h = open();
        let audio = vec![1u8, 2, 3, 4];
        h.call(
            ModuleName::Stt,
            "transcribe",
            serde_json::json!({ "dataB64": BASE64_STANDARD.encode(&audio) }),
        )
        .await
        .unwrap();
        h.call(
            ModuleName::Stt,
            "transcribe",
            serde_json::json!({
                "dataB64": BASE64_STANDARD.encode(&audio),
                "mime": "",
                "language": "",
            }),
        )
        .await
        .unwrap();
        h.call(
            ModuleName::Stt,
            "transcribe",
            serde_json::json!({
                "dataB64": BASE64_STANDARD.encode(&audio),
                "mime": "audio/ogg",
                "language": "id",
            }),
        )
        .await
        .unwrap();

        let seen = lock(&h.speech.seen_transcribe);
        assert_eq!(seen[0], (audio.clone(), "audio/wav".to_string(), None));
        // An empty string is ABSENT: `Some("")` reaches an external provider
        // as a language field it rejects.
        assert_eq!(seen[1], (audio.clone(), "audio/wav".to_string(), None));
        assert_eq!(
            seen[2],
            (audio, "audio/ogg".to_string(), Some("id".to_string()))
        );
    }

    #[tokio::test]
    async fn audio_that_is_not_base64_is_a_validation_error_rather_than_a_decode_panic() {
        let h = open();
        let err = h
            .call(
                ModuleName::Stt,
                "transcribe",
                serde_json::json!({ "dataB64": "not base64!!" }),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(err.message.contains("base64"), "{}", err.message);
        assert!(
            h.trace.entries().is_empty(),
            "undecodable audio was sent on"
        );
    }

    // --- db ------------------------------------------------------------------

    #[tokio::test]
    async fn a_query_comes_back_as_rows_of_named_columns() {
        let h = open();
        h.execute("CREATE TABLE t (a INTEGER, b TEXT)")
            .await
            .unwrap();
        let inserted = h
            .call(
                ModuleName::Db,
                "execute",
                serde_json::json!({
                    "sql": "INSERT INTO t (a, b) VALUES (?, ?), (?, ?)",
                    "params": [1, "one", 2, "two"],
                }),
            )
            .await
            .unwrap();
        assert_eq!(inserted["changes"], 2);
        assert_eq!(inserted["lastInsertRowId"], 2);

        let rows = h.query("SELECT a, b FROM t ORDER BY a").await.unwrap();
        assert_eq!(
            rows,
            serde_json::json!([{ "a": 1, "b": "one" }, { "a": 2, "b": "two" }])
        );
    }

    #[tokio::test]
    async fn a_value_json_cannot_carry_is_converted_rather_than_dropped() {
        let h = open();
        h.execute("CREATE TABLE t (big INTEGER, r REAL, blob BLOB, nil INTEGER)")
            .await
            .unwrap();
        h.execute("INSERT INTO t VALUES (9007199254740993, 1e400, x'0141', NULL)")
            .await
            .unwrap();
        let rows = h.query("SELECT * FROM t").await.unwrap();
        let row = &rows[0];
        // Past 2^53 the worker's `JSON.parse` would round it, so it travels as
        // a decimal string; Infinity has no JSON literal at all.
        assert_eq!(row["big"], "9007199254740993");
        assert_eq!(row["r"], Value::Null);
        assert_eq!(row["blob"], serde_json::json!({ "0": 1, "1": 65 }));
        assert_eq!(row["nil"], Value::Null);
    }

    #[tokio::test]
    async fn a_bind_parameter_json_has_no_column_type_for_is_refused() {
        let h = open();
        h.execute("CREATE TABLE t (a)").await.unwrap();
        for params in [
            serde_json::json!([{ "nested": 1 }]),
            serde_json::json!([[1, 2]]),
        ] {
            let err = h
                .call(
                    ModuleName::Db,
                    "execute",
                    serde_json::json!({ "sql": "INSERT INTO t VALUES (?)", "params": params }),
                )
                .await
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::ValidationError, "{params}");
        }
        // And `params` that is not an array at all.
        assert_eq!(
            h.call(
                ModuleName::Db,
                "execute",
                serde_json::json!({ "sql": "INSERT INTO t VALUES (?)", "params": "one" }),
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::ValidationError
        );
        // A boolean binds as SQLite's 1/0 rather than being refused.
        h.call(
            ModuleName::Db,
            "execute",
            serde_json::json!({ "sql": "INSERT INTO t VALUES (?)", "params": [true] }),
        )
        .await
        .unwrap();
        let rows = h.query("SELECT a FROM t").await.unwrap();
        assert_eq!(rows, serde_json::json!([{ "a": 1 }]));
    }

    #[tokio::test]
    async fn a_query_over_the_row_cap_fails_rather_than_answering_a_truncated_page() {
        let h = open();
        h.execute("CREATE TABLE t (n INTEGER)").await.unwrap();
        h.execute(
            "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 10001)
                        INSERT INTO t SELECT n FROM s",
        )
        .await
        .unwrap();

        let err = h.query("SELECT n FROM t").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError);

        // Exactly at the cap is fine: the bound is "more than".
        let rows = h.query("SELECT n FROM t LIMIT 10000").await.unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 10_000);
    }

    #[tokio::test]
    async fn the_sql_screen_runs_before_the_database_is_even_opened() {
        // Opening creates the file and its WAL, so a refused statement would
        // otherwise leave a database behind.
        let h = open();
        let err = h
            .query("ATTACH DATABASE '/tmp/x.db' AS x")
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
        assert!(
            lock(&h.broker.dbs).is_empty(),
            "a refused statement still opened the extension database"
        );
        assert!(
            !h.broker.paths.extension_data_dir("ext").exists(),
            "a refused statement still created the data directory"
        );
    }

    #[tokio::test]
    async fn each_extension_gets_its_own_file_and_never_the_cores() {
        let h = open();
        h.execute("CREATE TABLE mine (a)").await.unwrap();
        let file = h.broker.paths.extension_data_dir("ext").join("data.sqlite");
        assert!(file.exists(), "no per-extension database was created");
        assert_ne!(file, h.broker.paths.db_file());
    }

    #[tokio::test]
    async fn the_open_handle_cache_is_bounded_and_evicts_the_least_recently_used() {
        assert_eq!(EXTENSION_DB_MAX_OPEN, 16, "the shipped cap moved");
        let h = open();
        // Literal 16, not the constant, so the count cannot scale with it.
        for n in 0..16 {
            h.broker.extension_db(&format!("e{n}")).unwrap();
        }
        assert_eq!(lock(&h.broker.dbs).len(), 16);

        // Touch the oldest, so it is no longer the eviction candidate.
        h.broker.extension_db("e0").unwrap();
        h.broker.extension_db("overflow").unwrap();

        let open_ids: Vec<String> = lock(&h.broker.dbs)
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        assert_eq!(open_ids.len(), 16, "the cap did not hold at 17 extensions");
        assert!(
            open_ids.contains(&"e0".to_string()),
            "a touched handle was evicted"
        );
        assert!(
            !open_ids.contains(&"e1".to_string()),
            "the least recently used handle survived: {open_ids:?}"
        );
        assert_eq!(open_ids.last().unwrap(), "overflow");
    }

    #[tokio::test]
    async fn an_evicted_or_closed_database_still_has_its_data_on_reopen() {
        let h = open();
        h.execute("CREATE TABLE t (a)").await.unwrap();
        h.execute("INSERT INTO t VALUES (7)").await.unwrap();

        h.broker.close_extension_db("ext");
        assert!(lock(&h.broker.dbs).is_empty(), "the handle was not dropped");

        let rows = h.query("SELECT a FROM t").await.unwrap();
        assert_eq!(rows, serde_json::json!([{ "a": 7 }]));
    }

    #[tokio::test]
    async fn an_extension_database_answers_a_stuck_write_in_a_second_rather_than_five() {
        let h = open();
        let db = h.broker.extension_db("ext").unwrap();
        let timeout: i64 = db
            .call_blocking(|conn| Ok(conn.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(timeout, 1_000);
        let journal: String = db
            .call_blocking(|conn| Ok(conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(journal, "wal");
    }

    #[tokio::test]
    async fn a_declared_database_still_needs_no_grant_row_to_be_used() {
        // `database: true` is the whole authorisation; there are no grants.
        let mut facts = open_facts();
        facts.grants = Vec::new();
        facts.required = Vec::new();
        let h = Harness::new(facts, &[]);
        h.execute("CREATE TABLE t (a)").await.unwrap();
        assert!(
            h.prompter.asked().is_empty(),
            "a declared database prompted"
        );
    }
}
