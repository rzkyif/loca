//! The `ToolHost` adapter: the one type that knows about extensions, MCP and
//! the LLM at once.
//!
//! **Orchestrator-owned, and it has to be.** `chat` declares [`ToolHost`] and
//! must not import the three subsystems that answer it; none of those three can
//! name the other two without a dependency edge that does not otherwise exist.
//! So the join lives here, at the top level, next to [`crate::boot`] which is the
//! only other place that knows how every package's handle is built. It mirrors
//! the oracle's `packages/loca-core/src/host/deno-tool-host.ts`, which exists for
//! the same reason.
//!
//! There is deliberately **no policy in this file**. Every method is a single
//! call into the package that owns the decision. If a method here starts
//! branching, the branch belongs in the owning package: this type is a wiring
//! harness, and a harness that decides things is a fourth place to look when
//! behaviour surprises someone.

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::chat::deps::{
    McpTokens, Phase1Options, Phase1Result, ToolCallController, ToolCallEvent, ToolCallSpec,
    ToolEmbedding, ToolHost, ToolLogLevel,
};
use crate::contract::api::{ScheduledPromptDraft, Tool, ToolDescriptor};
use crate::error::Result;
use crate::extensions::tool_filter::{Phase2Completion, Phase2Request};
use crate::extensions::worker::call::{CallController, CallEvent, ToolCallStart};
use crate::extensions::worker::protocol::{ChatContext, LogLevel};
use crate::extensions::Extensions;
use crate::llm::{LlmEndpointConfig, Overrides, SingleShotOptions};

pub struct HostedTools {
    extensions: Extensions,
    mcp: crate::mcp::Mcp,
    /// For the phase-2 relevance pass, and nothing else. It runs on the
    /// endpoint the turn hands in rather than resolving one, so no settings
    /// handle is needed here: a second resolution is a second answer waiting to
    /// happen.
    llm: crate::llm::Llm,
}

impl HostedTools {
    pub fn new(extensions: Extensions, mcp: crate::mcp::Mcp, llm: crate::llm::Llm) -> Self {
        Self {
            extensions,
            mcp,
            llm,
        }
    }
}

/// Translates one in-flight call from the extension supervisor's vocabulary into
/// the orchestrator's.
///
/// The two enums are near-identical by design and are still kept apart: `chat`
/// would otherwise import `extensions::`, and the whole point of the seam is
/// that the orchestrator runs against a core with no extension supervisor at all
/// (`NoTools`), which is every mobile build.
struct ControllerBridge(CallController);

#[async_trait]
impl ToolCallController for ControllerBridge {
    async fn done(&self) -> Result<Value> {
        self.0.done().await
    }

    fn cancel(&self) {
        self.0.cancel();
    }

    fn respond_ask_user(
        &self,
        request_id: &str,
        answers: Vec<crate::contract::api::AskUserAnswer>,
    ) {
        self.0.respond_ask_user(request_id, answers);
    }

    fn respond_permission(&self, request_id: &str, allow: bool) {
        self.0.respond_permission(request_id, allow);
    }

    fn respond_schedule(
        &self,
        request_id: &str,
        accepted: bool,
        draft: Option<ScheduledPromptDraft>,
    ) {
        self.0.respond_schedule(request_id, accepted, draft);
    }

    fn has_pending_schedule(&self, request_id: &str) -> bool {
        self.0.has_pending_schedule(request_id)
    }
}

/// The phase-2 completion, on the endpoint the turn is already using.
///
/// Everything about the completion except where it runs was decided by the tool
/// filter and travels in the request. What this adds is the one thing neither
/// of the two packages can supply: `extensions::` names no LLM type at all, and
/// `chat` holds the endpoint but must not know that a tool filter is what wants
/// it.
struct RoutedCompletion<'a> {
    llm: &'a crate::llm::Llm,
    endpoint: &'a LlmEndpointConfig,
}

#[async_trait]
impl Phase2Completion for RoutedCompletion<'_> {
    async fn complete(&self, req: Phase2Request) -> Result<String> {
        // Unqueued, like every other utility completion. Queued, a filter
        // deciding which tools a turn may use would wait behind the turn it is
        // deciding for, which is a deadlock wearing the costume of slowness.
        self.llm
            .single_shot(SingleShotOptions {
                system_prompt: req.system_prompt,
                user_message: req.user_message,
                endpoint: self.endpoint.clone(),
                overrides: Overrides {
                    temperature: Some(req.temperature),
                    top_p: None,
                    max_tokens: Some(req.max_tokens),
                    reasoning_budget: Some(req.reasoning_budget),
                },
            })
            .await
            .map_err(Into::into)
    }
}

fn log_level(level: LogLevel) -> ToolLogLevel {
    match level {
        LogLevel::Debug => ToolLogLevel::Debug,
        LogLevel::Info => ToolLogLevel::Info,
        LogLevel::Warn => ToolLogLevel::Warn,
        LogLevel::Error => ToolLogLevel::Error,
    }
}

/// `None` for the one event the orchestrator has no shape for.
///
/// [`CallEvent::StderrLog`] is a worker's raw stderr line. It reaches the core
/// log through the supervisor's own path and is deliberately NOT forwarded into
/// a chat turn: the orchestrator's `Log` variant is the tool's structured
/// `tool.log`, which an extension author chose to emit, and putting a Deno stack
/// trace in the same channel would put unowned text in front of the user.
fn tool_event(event: CallEvent) -> Option<ToolCallEvent> {
    Some(match event {
        CallEvent::Progress {
            progress,
            label,
            description,
        } => ToolCallEvent::Progress {
            progress,
            label,
            description,
        },
        CallEvent::AskUserRequest {
            request_id,
            questions,
        } => ToolCallEvent::AskUserRequest {
            request_id,
            questions,
        },
        CallEvent::PermissionRequest {
            request_id,
            permission,
            resource,
            api_name,
            declared,
            reason,
        } => ToolCallEvent::PermissionRequest {
            request_id,
            permission,
            resource,
            api_name,
            declared,
            reason,
        },
        CallEvent::ScheduleRequest { request_id, draft } => {
            ToolCallEvent::ScheduleRequest { request_id, draft }
        }
        CallEvent::Log { level, message } => ToolCallEvent::Log {
            level: log_level(level),
            message,
        },
        CallEvent::Display { content } => ToolCallEvent::Display { content },
        CallEvent::ToolCancelled => ToolCallEvent::Cancelled,
        CallEvent::StderrLog { .. } => return None,
    })
}

#[async_trait]
impl ToolHost for HostedTools {
    // --- catalog -------------------------------------------------------------

    async fn installed_extension_tools(&self) -> Vec<Tool> {
        self.extensions
            .registry()
            .list_all_tools()
            .await
            .unwrap_or_default()
    }

    async fn get_tool(&self, tool_id: &str) -> Option<Tool> {
        self.extensions
            .registry()
            .get_tool(tool_id)
            .await
            .ok()
            .flatten()
    }

    async fn list_mcp_tools(&self) -> Vec<Tool> {
        self.mcp.list_all_tools().await.unwrap_or_default()
    }

    async fn is_mcp_server(&self, id: &str) -> bool {
        // `get` is the registry lookup, matching the oracle's
        // `mcpRegistry().get(id) !== undefined` (`deno-tool-host.ts:32`). An
        // error is not "yes": a DB failure must not route a tool call at the MCP
        // client, which would report a confusing server error instead of running
        // the extension tool.
        self.mcp.get(id).await.is_ok()
    }

    async fn load_tool_embeddings(&self) -> HashMap<String, ToolEmbedding> {
        // Two structurally identical types kept apart on purpose: the registry's
        // is a storage row, the orchestrator's is what ranking consumes. The
        // conversion is the seam.
        self.extensions
            .registry()
            .load_all_embeddings()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(id, stored)| {
                (
                    id,
                    ToolEmbedding {
                        vector: stored.vector,
                        source_hash: stored.source_hash,
                    },
                )
            })
            .collect()
    }

    // --- relevance -----------------------------------------------------------

    async fn filter_phase1(&self, query_vector: &[f32], options: Phase1Options) -> Phase1Result {
        // `phase1_with` rather than `phase1`, because the always-available
        // bypass is the caller's decision and the orchestrator carries it on
        // `Phase1Options`. Using `phase1` would hard-code `true` and silently
        // ignore the field.
        let response = self
            .extensions
            .phase1_with(
                query_vector,
                Some(options.top_k),
                options.query_text.as_deref(),
                options.include_always_available,
            )
            .await;
        // Ranking that fails is "nothing ranked", never "everything exposed":
        // an error here must not widen the tool surface of a turn.
        match response {
            Ok(response) => Phase1Result {
                candidates: response.candidates,
                always_available: response.always_available,
                name_matched: response.name_matched,
            },
            Err(err) => {
                tracing::warn!(target: "loca::tools", "tool relevance phase 1 failed: {err}");
                Phase1Result::default()
            }
        }
    }

    async fn filter_phase2(
        &self,
        query: &str,
        candidates: Vec<ToolDescriptor>,
        endpoint: &crate::llm::LlmEndpointConfig,
        reasoning_budget: u32,
    ) -> Vec<ToolDescriptor> {
        // The LLM stops here: what crosses into `extensions::` is a prompt and
        // an allowance, both of which the filter itself decided.
        crate::extensions::tool_filter::phase2(
            &RoutedCompletion {
                llm: &self.llm,
                endpoint,
            },
            query,
            candidates,
            reasoning_budget,
        )
        .await
    }

    // --- execution -----------------------------------------------------------

    async fn verify_tool_fresh(&self, extension_id: &str) -> Result<()> {
        self.extensions.registry().verify_fresh(extension_id).await
    }

    fn validate_tool_args(&self, tool: &Tool, arguments_json: &str) -> Result<String> {
        self.extensions.validate_tool_args(tool, arguments_json)
    }

    async fn start_tool_call(
        &self,
        spec: ToolCallSpec,
        events: mpsc::UnboundedSender<ToolCallEvent>,
    ) -> Result<Box<dyn ToolCallController>> {
        // The supervisor speaks `CallEvent`; the orchestrator speaks
        // `ToolCallEvent`. A forwarding task rather than a shared channel type,
        // so neither module has to name the other's enum.
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<CallEvent>();
        tokio::spawn(async move {
            while let Some(event) = raw_rx.recv().await {
                if let Some(mapped) = tool_event(event) {
                    // The receiver going away means the turn ended; stop
                    // forwarding rather than accumulating for nobody.
                    if events.send(mapped).is_err() {
                        break;
                    }
                }
            }
        });
        // `ToolCallSpec` carries the whole `Tool` because the orchestrator has
        // it in hand; the supervisor wants the two fields it dispatches on and
        // the chat context. The rest of the row is the supervisor's own, read
        // from its registry, so passing it here would be handing the
        // orchestrator's copy authority over what runs.
        let controller = self
            .extensions
            .start_tool_call(
                ToolCallStart {
                    extension_id: spec.extension_id,
                    tool_name: spec.tool.name,
                    fn_export: spec.tool.fn_export,
                    arguments_json: spec.arguments_json,
                    chat_context: ChatContext {
                        user_message: spec.user_message,
                        session_id: spec.session_id,
                        locale: spec.locale,
                    },
                },
                raw_tx,
            )
            .await?;
        Ok(Box::new(ControllerBridge(controller)))
    }

    async fn call_mcp_tool(
        &self,
        server_id: &str,
        name: &str,
        args: Map<String, Value>,
    ) -> Result<Value> {
        self.mcp.call_tool(server_id, name, args).await
    }

    // --- tokens --------------------------------------------------------------

    async fn resolve_mcp_tokens(&self, text: &str) -> McpTokens {
        // Two identical shapes kept apart so `chat` names no `mcp::` type; the
        // orchestrator's own doc on `McpTokens` says so.
        let resolved = self.mcp.resolve_tokens(text).await;
        McpTokens {
            block: resolved.block,
            claimed: resolved.claimed,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::contract::api::{DisplayContent, PermissionKind};

    use super::*;

    fn progress() -> CallEvent {
        CallEvent::Progress {
            progress: 0.5,
            label: Some("halfway".to_string()),
            description: None,
        }
    }

    #[test]
    fn every_call_event_reaches_the_turn_as_its_counterpart() {
        // The compiler proves the match is exhaustive over the SOURCE and
        // nothing proves each arm picked the right target. A `Progress` mapped
        // to `Log` would compile, and would show a user a log line every time a
        // tool reported progress.
        assert!(matches!(
            tool_event(progress()),
            Some(ToolCallEvent::Progress {
                progress: 0.5,
                label: Some(label),
                description: None,
            }) if label == "halfway"
        ));
        assert!(matches!(
            tool_event(CallEvent::AskUserRequest {
                request_id: "r1".to_string(),
                questions: Vec::new(),
            }),
            Some(ToolCallEvent::AskUserRequest { request_id, .. }) if request_id == "r1"
        ));
        assert!(matches!(
            tool_event(CallEvent::PermissionRequest {
                request_id: "r2".to_string(),
                permission: PermissionKind::Read,
                resource: "/etc/hosts".to_string(),
                api_name: None,
                declared: true,
                reason: None,
            }),
            Some(ToolCallEvent::PermissionRequest {
                permission: PermissionKind::Read,
                resource,
                declared: true,
                ..
            }) if resource == "/etc/hosts"
        ));
        assert!(matches!(
            tool_event(CallEvent::Log {
                level: LogLevel::Warn,
                message: "careful".to_string(),
            }),
            Some(ToolCallEvent::Log {
                level: ToolLogLevel::Warn,
                message,
            }) if message == "careful"
        ));
        assert!(matches!(
            tool_event(CallEvent::Display {
                content: DisplayContent::Markdown {
                    markdown: "hi".to_string(),
                },
            }),
            Some(ToolCallEvent::Display { .. })
        ));
        assert!(matches!(
            tool_event(CallEvent::ToolCancelled),
            Some(ToolCallEvent::Cancelled)
        ));
    }

    #[test]
    fn a_workers_raw_stderr_does_not_become_a_chat_log_line() {
        // The one event with no counterpart. Forwarding it would put a Deno
        // stack trace in the channel an extension author's own `tool.log`
        // messages arrive on, in front of the user.
        assert!(tool_event(CallEvent::StderrLog {
            line: "error: Uncaught (in promise) NotCapable".to_string(),
        })
        .is_none());
    }

    #[test]
    fn the_four_log_levels_keep_their_meaning() {
        // A wire value, so a swap here mislabels every tool log line rather
        // than failing anything.
        assert_eq!(log_level(LogLevel::Debug), ToolLogLevel::Debug);
        assert_eq!(log_level(LogLevel::Info), ToolLogLevel::Info);
        assert_eq!(log_level(LogLevel::Warn), ToolLogLevel::Warn);
        assert_eq!(log_level(LogLevel::Error), ToolLogLevel::Error);
    }
}
