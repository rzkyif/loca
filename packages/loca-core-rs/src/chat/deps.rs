//! The seams a chat turn needs from packages that do not exist yet, or whose
//! shape is theirs rather than ours.
//!
//! Declared here, in the consumer, following `sessions::ChatEngine`,
//! `memories::MemoryIndexer` and `storage::deps::ModelLocks`: the consumer
//! declares the seam so its own code compiles and is testable before the
//! producer exists, and a test injects a scripted implementation instead of a
//! subprocess or a language model.
//!
//! Every trait here has an `Unwired` implementation. That is not a stub for
//! convenience: each one is the correct degraded behaviour for a core that
//! genuinely lacks the subsystem, which on mobile is the shipping
//! configuration rather than a temporary state.

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

/// One event from a running extension tool call.
///
/// Ports `core-engine/src/services/tool-host.ts:25-41`. The orchestrator maps
/// each to a `ServerToClientFrame`; nothing else consumes them.
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

/// `tool-host.ts:38`'s four levels. Not the tracing `Level`: this one is a wire
/// value carried on `tool.log`, and it is spelled by the contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolLogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// What the orchestrator hands the host to start one call.
///
/// Ports `tool-host.ts:57-62`. `session_id` is `Option` because the module
/// broker can run a tool outside a chat session, which is the case the
/// TypeScript spells `sessionId: string | null`.
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
///
/// Ports `tool-host.ts:46-54`. The `done` half of the TypeScript's promise is a
/// method rather than a field, because a `Box<dyn Trait>` cannot hand out an
/// owned future from a shared reference without one.
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
    ///
    /// The guard that makes `schedule.confirm_response` idempotent: without it a
    /// replayed response persists a second scheduled prompt. See
    /// `chat.ts:191-194`.
    fn has_pending_schedule(&self, request_id: &str) -> bool;
}

/// One tool's persisted relevance vector, for phase-1 cosine.
///
/// `source_hash` is what makes a vector embedded by a different model read as
/// "not embedded" rather than being silently mis-ranked (`tool-filter.ts:79-83`).
#[derive(Clone, Debug)]
pub struct ToolEmbedding {
    pub vector: Vec<f32>,
    pub source_hash: String,
}

/// What `@resource` and `/prompt` expansion produced for one turn.
///
/// Mirrors `mcp::tokens::TokenResult`; restated here so `chat` does not name
/// `mcp::` at all.
#[derive(Debug, Default)]
pub struct McpTokens {
    pub block: Option<String>,
    /// Lowercased slugs this pass claimed, handed to the memory expander so a
    /// slug naming both an MCP resource and a memory expands once, MCP winning.
    pub claimed: std::collections::HashSet<String>,
}

/// The unified tool catalog and execution surface.
///
/// Ports `core-engine/src/services/tool-host.ts:64-100`, and it is deliberately
/// ONE trait over both providers rather than two. The chat graph branches on
/// provider in exactly one place, [`Self::is_mcp_server`]
/// (`chat-tool-dispatch.ts:101`); everywhere else an MCP tool and an extension
/// tool are the same thing. Splitting the trait would move that branch into
/// every caller.
///
/// The production implementation is an adapter over `Option<Extensions>` plus
/// `Mcp`, built in `boot`, mirroring the oracle's own
/// `core/src/host/deno-tool-host.ts`. `chat` never names either module, which is
/// what lets the mobile core supply a reduced implementation (remote MCP only)
/// rather than scattering `if let Some(extensions)` through the hop loop.
#[async_trait]
pub trait ToolHost: Send + Sync {
    /// Whether this host can actually offer tools.
    ///
    /// Exists for the boot-seam suite (D-165) and answers a question no catalog
    /// read can: every method of [`NoTools`] returns empty, which is
    /// INDISTINGUISHABLE from a live host on a core with nothing installed. So
    /// "did chat get a real tool host" cannot be observed by calling the trait,
    /// and the regression it guards (the model offered no tools at all while
    /// `/api/v1/tools` lists a full catalog) is silent by construction.
    ///
    /// Defaulted to `true` so a real implementation cannot forget it; the two
    /// non-hosts override.
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
    /// client rather than the worker sandbox.
    ///
    /// **Async where the oracle is sync** (`deno-tool-host.ts:32` answers from
    /// an in-memory `mcpRegistry()` map). The Rust MCP registry is DB-backed, so
    /// a synchronous answer would need a parallel in-memory cache of server ids
    /// whose invalidation is a new bug surface, bought for nothing: every call
    /// site is already in async context.
    async fn is_mcp_server(&self, id: &str) -> bool;

    /// The persisted relevance vectors, keyed by tool id. One batched read
    /// rather than a round trip per tool.
    async fn load_tool_embeddings(&self) -> HashMap<String, ToolEmbedding>;

    // --- relevance ---
    /// Phase 1: cosine over the persisted vectors, top-K, plus the
    /// always-available and directly-named sets.
    ///
    /// Lives on the host rather than in `chat` because it ranks rows only the
    /// tool owner can read. See D-057.
    async fn filter_phase1(&self, query_vector: &[f32], options: Phase1Options) -> Phase1Result;

    /// Phase 2: ask the model which phase-1 candidates are plausibly useful.
    /// Fails OPEN, returning its input, so a flaky model cannot leave a turn
    /// with no tools.
    ///
    /// The turn's OWN endpoint, already resolved, rather than the route it came
    /// from. The oracle resolves the route a second time here
    /// (`chat-tool-selection.ts:164`) and gets the same answer; in Rust it would
    /// not, because the settings record a turn carries has the schema defaults
    /// resolved underneath it and `Llm::resolve_endpoint` must not be handed
    /// one. Passing what the turn already holds removes both the second load
    /// and the chance of filtering tools against a different provider than the
    /// turn then streams from.
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
    /// The TypeScript passes an `onEvent` callback invoked synchronously from
    /// the worker's message pump. A channel instead, because the orchestrator's
    /// handler must await (a `display` event writes to the session store), and a
    /// callback that can await would force a boxed future through the host's
    /// whole event path.
    ///
    /// **Async, where the oracle is synchronous**, and for the same reason
    /// [`Self::is_mcp_server`] is: the Rust side genuinely is. Starting a call
    /// may read the registry out of SQLite, bind a loopback control socket and
    /// spawn a process, where `Deno.Command.spawn()` returns at once against an
    /// in-memory registry. Keeping the signature synchronous would mean either
    /// blocking inside the runtime or detaching the spawn onto a task, and the
    /// second one loses what the caller needs most: a spawn that fails must
    /// fail THIS call, visibly, rather than arriving later as a worker that
    /// never booted.
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

/// Phase-1 inputs. Ports `tool-filter.ts:28-38`.
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

/// Phase-1 outputs. Ports `tool-filter.ts:40-48`.
#[derive(Clone, Debug, Default)]
pub struct Phase1Result {
    pub candidates: Vec<ToolDescriptor>,
    pub always_available: Vec<ToolDescriptor>,
    /// Tools the query names directly. Force-included by the caller ahead of the
    /// cosine candidates and exempt from the cap, so naming a tool by hand
    /// always exposes it even with no fresh embedding.
    pub name_matched: Vec<ToolDescriptor>,
}

/// A core with no tool subsystem at all.
///
/// The mobile shipping configuration for extensions, and what a turn-level test
/// installs when tools are not what it is testing. Every catalog read is empty,
/// which makes a turn run with no tools rather than fail: the same thing a
/// desktop core with no extensions installed already does.
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
        // Fails open, exactly as a real phase 2 does when the model is
        // unreachable. Returning an empty set here would make "no filter" and
        // "filter rejected everything" indistinguishable.
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
        // Unreachable through the catalog above, since no tool is ever offered.
        // An error rather than a silent no-op, because a call arriving here
        // means the catalog and the dispatcher disagree.
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

/// Persist a user-confirmed scheduled prompt.
///
/// D10's, and one method because that is the whole of what a chat turn asks:
/// `chat.ts:199`'s single `host().status.createScheduledPrompt` call.
///
/// Synchronous and fallible, and both matter. The caller runs it INSIDE the
/// response handler, before unblocking the tool, so the tool's "scheduled"
/// report is only ever sent for a draft that was actually stored
/// (`chat.ts:196-206`).
pub trait SchedulePersist: Send + Sync {
    fn create(&self, client_id: &str, draft: &ScheduledPromptDraft) -> Result<()>;
}

/// A core with no scheduler.
///
/// Refuses rather than silently accepting: a confirm that reports success and
/// stores nothing is the one outcome the persist-before-unblock ordering exists
/// to prevent.
pub struct NoSchedule;

impl SchedulePersist for NoSchedule {
    fn create(&self, _client_id: &str, _draft: &ScheduledPromptDraft) -> Result<()> {
        Err(AppError::new(
            ErrorCode::ServerUnavailable,
            "this core has no scheduled-prompt store",
        ))
    }
}

/// The local model's idle lifecycle.
///
/// N2's, over what is today `core/src/services/llm-idle.ts`. A turn touches it
/// three times: cancel a pending unload as it starts (`chat.ts:126`), reload a
/// model that idle-unloading stopped (`:294`), and arm the timer when the last
/// turn ends (`:141`).
///
/// Not on this trait: the LLM queue depth. D3 already publishes it through
/// `Llm::attach_status`, and a second publisher would race it.
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

/// A core with no local inference, which is every core until N2 lands and every
/// mobile core that uses an external provider.
pub struct NoIdle;

impl ModelIdle for NoIdle {
    fn note_activity(&self) {}

    fn on_turn_end(&self, _active_streams: u32) {}

    fn ensure_loaded<'a>(&'a self, _settings: &'a SettingsRecord) -> BoxFuture<'a, Result<()>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// The three seams together, so a caller passes one value rather than three and
/// a new seam does not change every construction site.
#[derive(Clone)]
pub struct ChatDeps {
    pub tools: Arc<dyn ToolHost>,
    pub schedule: Arc<dyn SchedulePersist>,
    pub idle: Arc<dyn ModelIdle>,
}

impl ChatDeps {
    /// Every seam absent. What a store-level test uses, and what a core with
    /// neither extensions nor a scheduler genuinely has.
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
