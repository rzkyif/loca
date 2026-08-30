//! The seams a chat turn needs from other subsystems, declared in the consumer
//! so a test can inject a scripted implementation instead of a subprocess or a
//! language model. Each `No*` implementation is not a stub: it is the correct
//! degraded behaviour of a core that genuinely lacks that subsystem.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::contract::api::{
    AskUserAnswer, AskUserQuestion, DisplayContent, PermissionKind, ScheduledPromptDraft, Tool,
    ToolDescriptor,
};
use crate::error::{AppError, ErrorCode, Result};
use crate::settings::SettingsRecord;

/// One event from a running extension tool call. The orchestrator maps each to
/// a `ServerToClientFrame`; nothing else consumes them.
#[derive(Clone, Debug)]
pub enum ToolCallEvent {
    Progress {
        progress: f64,
        label: Option<String>,
        description: Option<String>,
    },
    AskUserRequest {
        request_id: String,
        questions: Vec<AskUserQuestion>,
    },
    PermissionRequest {
        request_id: String,
        permission: PermissionKind,
        resource: String,
        api_name: Option<String>,
        declared: bool,
        reason: Option<String>,
    },
    ScheduleRequest {
        request_id: String,
        draft: ScheduledPromptDraft,
    },
    Log {
        level: ToolLogLevel,
        message: String,
    },
    Display {
        content: DisplayContent,
    },
    Cancelled,
}

/// Not the tracing `Level`: a wire value carried on `tool.log`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolLogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// What the orchestrator hands the host to start one call. `session_id` is
/// `Option` because the module broker can run a tool outside a chat session.
pub struct ToolCallSpec {
    pub extension_id: String,
    pub tool: Tool,
    /// Already validated and defaulted by `validate_tool_args`, never raw model
    /// output.
    pub arguments_json: String,
    pub user_message: String,
    pub session_id: Option<String>,
    pub locale: Option<String>,
}

/// A handle on one in-flight extension tool call.
#[async_trait]
pub trait ToolCallController: Send + Sync {
    /// The tool's result, or the error it failed with. Resolves once per call;
    /// calling it twice is a programming error and returns the same terminal
    /// state.
    async fn done(&self) -> Result<Value>;
    fn cancel(&self);
    fn respond_ask_user(&self, request_id: &str, answers: Vec<AskUserAnswer>);
    fn respond_permission(&self, request_id: &str, allow: bool);
    fn respond_schedule(
        &self,
        request_id: &str,
        accepted: bool,
        draft: Option<ScheduledPromptDraft>,
    );
    /// Whether `request_id` names the schedule confirm that is currently open.
    /// The guard that makes `schedule.confirm_response` idempotent: without it a
    /// replayed response persists a second scheduled prompt.
    fn has_pending_schedule(&self, request_id: &str) -> bool;
}

/// One tool's persisted relevance vector, for phase-1 cosine. `source_hash` is
/// what makes a vector embedded by a different model read as "not embedded"
/// rather than being silently mis-ranked.
#[derive(Clone, Debug)]
pub struct ToolEmbedding {
    pub vector: Vec<f32>,
    pub source_hash: String,
}

/// What `@resource` and `/prompt` expansion produced for one turn. Mirrors
/// `mcp::tokens::TokenResult`, restated so `chat` never names `mcp::`.
#[derive(Debug, Default)]
pub struct McpTokens {
    pub block: Option<String>,
    /// Lowercased slugs this pass claimed, handed to the memory expander so a
    /// slug naming both an MCP resource and a memory expands once, MCP winning.
    pub claimed: std::collections::HashSet<String>,
}

/// The unified tool catalog and execution surface, deliberately ONE trait over
/// both providers: the chat graph branches on provider in exactly one place,
/// [`Self::is_mcp_server`]. Splitting it would move that branch into every
/// caller. Boot supplies an adapter over `Option<Extensions>` plus `Mcp`, so
/// `chat` names neither module and mobile can pass a reduced implementation.
#[async_trait]
pub trait ToolHost: Send + Sync {
    /// Whether this host can actually offer tools. Answers what no catalog read
    /// can: [`NoTools`] is indistinguishable from a live host on a core with
    /// nothing installed, so "did chat get a real tool host" is otherwise
    /// unobservable. Defaulted so a real implementation cannot forget it.
    fn is_live(&self) -> bool {
        true
    }

    // --- catalog ---
    /// Every tool of an installed extension, enabled AND disabled, with grants
    /// attached and OS-unsupported tools already filtered out. The caller
    /// applies its own enabled and exposable gating on top.
    async fn installed_extension_tools(&self) -> Vec<Tool>;

    /// One tool by `${extensionId}::${name}`.
    async fn get_tool(&self, tool_id: &str) -> Option<Tool>;

    /// Every enabled MCP server's tools, in the same shape.
    async fn list_mcp_tools(&self) -> Vec<Tool>;

    /// Whether `id` names a known MCP server, so a call routes to the MCP
    /// client rather than the worker sandbox. Async because the registry is
    /// DB-backed and every call site is already in async context.
    async fn is_mcp_server(&self, id: &str) -> bool;

    /// The persisted relevance vectors, keyed by tool id. One batched read
    /// rather than a round trip per tool.
    async fn load_tool_embeddings(&self) -> HashMap<String, ToolEmbedding>;

    // --- relevance ---
    /// Phase 1: cosine over the persisted vectors, top-K, plus the
    /// always-available and directly-named sets. On the host rather than in
    /// `chat` because it ranks rows only the tool owner can read.
    async fn filter_phase1(&self, query_vector: &[f32], options: Phase1Options) -> Phase1Result;

    /// Phase 2: ask the model which phase-1 candidates are plausibly useful.
    /// Fails OPEN, returning its input, so a flaky model cannot leave a turn
    /// with no tools.
    ///
    /// Takes the turn's OWN resolved endpoint rather than re-resolving: the
    /// settings a turn carries already have schema defaults underneath them,
    /// which `Llm::resolve_endpoint` must not be handed, and re-resolving could
    /// filter against a different provider than the turn streams from.
    async fn filter_phase2(
        &self,
        query: &str,
        candidates: Vec<ToolDescriptor>,
        endpoint: &crate::llm::LlmEndpointConfig,
        reasoning_budget: u32,
    ) -> Vec<ToolDescriptor>;

    // --- execution ---
    /// Re-verify an extension's content hash before a call. Runs on EVERY call,
    /// so a reused warm worker is re-checked too.
    async fn verify_tool_fresh(&self, extension_id: &str) -> Result<()>;

    /// Validate and normalise model-emitted arguments against the tool's
    /// declared schema, filling defaults.
    fn validate_tool_args(&self, tool: &Tool, arguments_json: &str) -> Result<String>;

    /// Start a call in the sandbox. Events arrive on `events`.
    ///
    /// Async because starting a call reads SQLite, binds a control socket and
    /// spawns a process. Detaching that onto a task would lose what the caller
    /// needs most: a spawn that fails must fail THIS call, visibly, rather than
    /// arriving later as a worker that never booted.
    async fn start_tool_call(
        &self,
        spec: ToolCallSpec,
        events: mpsc::UnboundedSender<ToolCallEvent>,
    ) -> Result<Box<dyn ToolCallController>>;

    /// Call a tool on its MCP server. A straight request and response: MCP tools
    /// have no local permissions, progress, or askUser flow.
    async fn call_mcp_tool(
        &self,
        server_id: &str,
        name: &str,
        args: Map<String, Value>,
    ) -> Result<Value>;

    // --- tokens ---
    /// Expand `@resource` and `/prompt` references in the turn's user text.
    async fn resolve_mcp_tokens(&self, text: &str) -> McpTokens;
}

/// Phase-1 inputs.
#[derive(Clone, Debug)]
pub struct Phase1Options {
    /// Maximum candidates after the cosine sort. Always-available tools are
    /// added on top of this, so the effective list can exceed it.
    pub top_k: usize,
    pub include_always_available: bool,
    /// The raw user query. Tools whose name it names directly come back in
    /// `name_matched` for the caller to force-include.
    pub query_text: Option<String>,
}

/// Phase-1 outputs.
#[derive(Clone, Debug, Default)]
pub struct Phase1Result {
    pub candidates: Vec<ToolDescriptor>,
    pub always_available: Vec<ToolDescriptor>,
    /// Tools the query names directly. Force-included by the caller ahead of the
    /// cosine candidates and exempt from the cap, so naming a tool by hand
    /// always exposes it even with no fresh embedding.
    pub name_matched: Vec<ToolDescriptor>,
}

/// A core with no tool subsystem at all. Every catalog read is empty, so a turn
/// runs with no tools rather than failing.
pub struct NoTools;

#[async_trait]
impl ToolHost for NoTools {
    fn is_live(&self) -> bool {
        false
    }

    async fn installed_extension_tools(&self) -> Vec<Tool> {
        Vec::new()
    }

    async fn get_tool(&self, _tool_id: &str) -> Option<Tool> {
        None
    }

    async fn list_mcp_tools(&self) -> Vec<Tool> {
        Vec::new()
    }

    async fn is_mcp_server(&self, _id: &str) -> bool {
        false
    }

    async fn load_tool_embeddings(&self) -> HashMap<String, ToolEmbedding> {
        HashMap::new()
    }

    async fn filter_phase1(&self, _query_vector: &[f32], _options: Phase1Options) -> Phase1Result {
        Phase1Result::default()
    }

    async fn filter_phase2(
        &self,
        _query: &str,
        candidates: Vec<ToolDescriptor>,
        _endpoint: &crate::llm::LlmEndpointConfig,
        _reasoning_budget: u32,
    ) -> Vec<ToolDescriptor> {
        // Fails open, as a real phase 2 does when the model is unreachable.
        candidates
    }

    async fn verify_tool_fresh(&self, _extension_id: &str) -> Result<()> {
        Ok(())
    }

    fn validate_tool_args(&self, _tool: &Tool, arguments_json: &str) -> Result<String> {
        Ok(arguments_json.to_string())
    }

    async fn start_tool_call(
        &self,
        _spec: ToolCallSpec,
        _events: mpsc::UnboundedSender<ToolCallEvent>,
    ) -> Result<Box<dyn ToolCallController>> {
        // Unreachable via the empty catalog: an arrival here means the catalog
        // and the dispatcher disagree, so it must be visible.
        Err(AppError::new(
            ErrorCode::ServerUnavailable,
            "this core has no extension subsystem",
        ))
    }

    async fn call_mcp_tool(
        &self,
        _server_id: &str,
        _name: &str,
        _args: Map<String, Value>,
    ) -> Result<Value> {
        Err(AppError::new(
            ErrorCode::ServerUnavailable,
            "this core has no MCP subsystem",
        ))
    }

    async fn resolve_mcp_tokens(&self, _text: &str) -> McpTokens {
        McpTokens::default()
    }
}

/// Persist a user-confirmed scheduled prompt. Synchronous and fallible: the
/// caller runs it inside the response handler, before unblocking the tool, so
/// the tool's "scheduled" report is only sent for a draft that was stored.
pub trait SchedulePersist: Send + Sync {
    fn create(&self, client_id: &str, draft: &ScheduledPromptDraft) -> Result<()>;
}

/// A core with no scheduler. Refuses rather than silently accepting: a confirm
/// that reports success and stores nothing is what the ordering above prevents.
pub struct NoSchedule;

impl SchedulePersist for NoSchedule {
    fn create(&self, _client_id: &str, _draft: &ScheduledPromptDraft) -> Result<()> {
        Err(AppError::new(
            ErrorCode::ServerUnavailable,
            "this core has no scheduled-prompt store",
        ))
    }
}

/// The local model's idle lifecycle. A turn cancels a pending unload as it
/// starts, reloads a model idle-unloading stopped, and arms the timer when the
/// last turn ends. Queue depth is deliberately not here: `Llm::attach_status`
/// already publishes it and a second publisher would race it.
pub trait ModelIdle: Send + Sync {
    /// Cancel any pending idle-unload: a turn is about to use the model.
    fn note_activity(&self);

    /// Arm the idle-unload timer when the last active turn ends. A no-op unless
    /// `active_streams` is zero and the feature is on.
    fn on_turn_end(&self, active_streams: u32);

    /// Reload the local model if idle-unloading stopped it. A no-op for an
    /// external provider or a core with no local inference.
    fn ensure_loaded<'a>(&'a self, settings: &'a SettingsRecord) -> BoxFuture<'a, Result<()>>;
}

/// A core with no local inference.
pub struct NoIdle;

impl ModelIdle for NoIdle {
    fn note_activity(&self) {}

    fn on_turn_end(&self, _active_streams: u32) {}

    fn ensure_loaded<'a>(&'a self, _settings: &'a SettingsRecord) -> BoxFuture<'a, Result<()>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// The three seams together, so a new seam does not change every construction
/// site.
#[derive(Clone)]
pub struct ChatDeps {
    pub tools: Arc<dyn ToolHost>,
    pub schedule: Arc<dyn SchedulePersist>,
    pub idle: Arc<dyn ModelIdle>,
}

impl ChatDeps {
    /// Every seam absent.
    pub fn unwired() -> Self {
        Self {
            tools: Arc::new(NoTools),
            schedule: Arc::new(NoSchedule),
            idle: Arc::new(NoIdle),
        }
    }

    pub fn with_tools(mut self, tools: Arc<dyn ToolHost>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_schedule(mut self, schedule: Arc<dyn SchedulePersist>) -> Self {
        self.schedule = schedule;
        self
    }

    pub fn with_idle(mut self, idle: Arc<dyn ModelIdle>) -> Self {
        self.idle = idle;
        self
    }
}
