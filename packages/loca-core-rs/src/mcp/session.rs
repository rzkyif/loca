//! One live MCP conversation, and the seam that opens it.
//!
//! The manager is written against [`Session`] and [`Connector`] rather than
//! against a transport type, for the reason the TypeScript had a
//! `setTransportFactory` seam: a test needs a client linked to a real MCP
//! server without spawning a process or reaching a URL.
//!
//! # Cancellation is dropping the future
//!
//! The TypeScript threads the turn's `AbortSignal` into every request so a hung
//! server cannot stall a turn. In Rust the same guarantee is structural: drop
//! the future and the await is gone. So these methods take no signal, and a
//! caller that needs to abort selects over its own cancellation instead. Every
//! request additionally carries [`REQUEST_TIMEOUT`], which is the bound that
//! does not depend on a caller remembering.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rmcp::model::{
    CallToolRequestParams, ClientInfo, GetPromptRequestParams, Implementation,
    ReadResourceRequestParams,
};
use rmcp::service::{NotificationContext, Peer, RoleClient, RunningService};
use rmcp::ClientHandler;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::config::CORE_VERSION;
use crate::error::{AppError, ErrorCode, Result};

use super::registry::ServerConfig;
use crate::contract::api::McpPromptArg;

/// A single MCP request must not hang a turn forever if a server accepts the
/// connection and then never answers. Callers fold the failure into a failed
/// tool result or a dropped token rather than failing the turn.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The `initialize` exchange is a request like any other and needs the same
/// kind of bound, but it is the one request that does not go through
/// [`PeerSession::bounded`], because there is no peer until it finishes.
///
/// Nothing underneath supplies a bound: rmcp's `expect_response` loops on the
/// transport with no timeout, and the remote transport's `reqwest::Client` is
/// built without one. So an endpoint that accepts the TCP connection and never
/// answers, or a child process that starts and never speaks, would hold the
/// connect open for the life of the process.
///
/// 60s rather than [`REQUEST_TIMEOUT`] because that is what the specification
/// applies: `client.connect(transport)` in `packages/loca-core/src/mcp/manager.ts`
/// passes no options, so the MCP SDK falls back to its
/// `DEFAULT_REQUEST_TIMEOUT_MSEC` (`shared/protocol.js:8,712`).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

/// What a bounded handshake produced.
pub enum Handshake<T, E> {
    Ready(T),
    /// The transport or the server refused, in its own words.
    Failed(E),
    /// Nothing came back in time.
    TimedOut,
}

/// Run a transport's handshake under a bound.
///
/// The bound is a parameter rather than read from [`HANDSHAKE_TIMEOUT`]
/// directly so a test can drive a real black-holed socket to the timeout
/// without waiting a real minute for it.
pub async fn handshake<F, T, E>(timeout: Duration, future: F) -> Handshake<T, E>
where
    F: std::future::Future<Output = std::result::Result<T, E>>,
{
    match tokio::time::timeout(timeout, future).await {
        Ok(Ok(value)) => Handshake::Ready(value),
        Ok(Err(err)) => Handshake::Failed(err),
        Err(_) => Handshake::TimedOut,
    }
}

/// What the user is told when a server takes the connection and then says
/// nothing. Deliberately distinguishes "did not answer" from "refused", because
/// the two need different things done about them.
pub fn handshake_timeout_message(timeout: Duration) -> String {
    format!(
        "The server did not finish the MCP handshake within {}s.",
        timeout.as_secs()
    )
}

/// A tool as the server describes it.
#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Map<String, Value>,
}

/// A prompt as the server describes it.
#[derive(Clone, Debug)]
pub struct PromptDef {
    pub name: String,
    pub description: Option<String>,
    pub arguments: Vec<McpPromptArg>,
}

/// A resource as the server describes it.
#[derive(Clone, Debug)]
pub struct ResourceDef {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

/// One piece of a resource read. A blob carries no text, and the resolver says
/// so rather than inlining base64 into a prompt.
#[derive(Clone, Debug)]
pub struct ResourceChunk {
    pub text: Option<String>,
    pub is_blob: bool,
}

/// Which capability list a server said had changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Tools,
    Prompts,
    Resources,
}

/// Something a connection reported on its own, rather than in answer to a
/// request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// Re-fetch this one list.
    ListChanged(Capability),
    /// The service loop ended. Intentional or not: the manager decides, by
    /// checking whether this is still the active connection.
    Closed,
}

/// An open conversation with one server.
#[async_trait]
pub trait Session: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<ToolDef>>;
    async fn list_prompts(&self) -> Result<Vec<PromptDef>>;
    async fn list_resources(&self) -> Result<Vec<ResourceDef>>;
    async fn call_tool(&self, name: &str, args: Map<String, Value>) -> Result<Value>;
    async fn get_prompt(
        &self,
        name: &str,
        args: BTreeMap<String, String>,
    ) -> Result<Vec<PromptMessage>>;
    async fn read_resource(&self, uri: &str) -> Result<Vec<ResourceChunk>>;
    /// End the conversation. Idempotent, and best effort: a server that has
    /// already gone away is not an error to close.
    fn close(&self);
}

/// One message of a prompt's expansion, flattened to what the injector needs.
#[derive(Clone, Debug)]
pub struct PromptMessage {
    pub text: String,
}

/// A freshly opened connection: the session, plus the stream of things it will
/// report on its own.
pub struct Connected {
    pub session: Arc<dyn Session>,
    pub events: mpsc::UnboundedReceiver<SessionEvent>,
}

/// How the manager opens a connection. The production implementation dispatches
/// on the server's transport; a test substitutes an in-process server.
#[async_trait]
pub trait Connector: Send + Sync {
    async fn connect(&self, server: &ServerConfig) -> Result<Connected>;
    /// Whether this connector can run a local subprocess. Mobile cannot, so an
    /// stdio server is refused before anything is spawned, and M3's
    /// `capabilities.stdioMcp` reports this same value.
    fn stdio_supported(&self) -> bool;
}

/// What Core calls itself during the MCP handshake.
pub fn client_info() -> ClientInfo {
    // `ClientInfo` is `#[non_exhaustive]`, so it is built from its default and
    // then adjusted rather than written as a literal.
    let mut info = ClientInfo::default();
    info.client_info = Implementation::new("Loca", CORE_VERSION);
    info
}

/// The `ClientHandler` half: it exists to turn a server's `list_changed`
/// notifications into [`SessionEvent`]s the manager can act on.
#[derive(Clone)]
pub struct EventHandler {
    tx: mpsc::UnboundedSender<SessionEvent>,
}

impl EventHandler {
    fn send(&self, event: SessionEvent) {
        // An error here means the manager dropped the receiver, which happens
        // when the connection is already being torn down. Nothing to do.
        let _ = self.tx.send(event);
    }
}

impl ClientHandler for EventHandler {
    fn get_info(&self) -> ClientInfo {
        client_info()
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.send(SessionEvent::ListChanged(Capability::Tools));
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.send(SessionEvent::ListChanged(Capability::Prompts));
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.send(SessionEvent::ListChanged(Capability::Resources));
    }
}

/// Take ownership of a started service and hand back the two halves the
/// manager wants: a [`Session`] to make requests through, and the event stream.
///
/// The `RunningService` is moved into a task that awaits its termination, which
/// is what turns a dropped connection into a [`SessionEvent::Closed`]. It has
/// to be a task because `waiting()` consumes the service; the `Peer` is a
/// cheap clonable handle over the same loop, so requests keep working from the
/// outside while the task holds the service.
pub fn adopt<S>(
    service: RunningService<RoleClient, S>,
    tx: mpsc::UnboundedSender<SessionEvent>,
    teardown: Option<Teardown>,
) -> Arc<dyn Session>
where
    S: rmcp::service::Service<RoleClient>,
{
    let peer = service.peer().clone();
    let cancel = std::sync::Mutex::new(Some(service.cancellation_token()));
    tokio::spawn(async move {
        let _ = service.waiting().await;
        let _ = tx.send(SessionEvent::Closed);
    });
    Arc::new(PeerSession {
        peer,
        cancel,
        teardown: std::sync::Mutex::new(teardown),
    })
}

/// What a transport needs run once the protocol close is done, and what it
/// needs kept alive until then. A stdio connection passes the child handle in
/// here: the closure owns it, so the process lives exactly as long as the
/// session.
pub type Teardown = Box<dyn FnOnce() + Send + Sync>;

/// Build the handler and the channel the manager reads.
pub fn event_channel() -> (EventHandler, mpsc::UnboundedReceiver<SessionEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (EventHandler { tx: tx.clone() }, rx)
}

/// The sender half again, for [`adopt`], which needs to report the close on the
/// same channel the handler reports list changes on.
pub fn sender(handler: &EventHandler) -> mpsc::UnboundedSender<SessionEvent> {
    handler.tx.clone()
}

/// A [`Session`] over an rmcp peer.
struct PeerSession {
    peer: Peer<RoleClient>,
    /// Taken by the first `close`, so cancelling twice is not an error.
    cancel: std::sync::Mutex<Option<rmcp::service::RunningServiceCancellationToken>>,
    teardown: std::sync::Mutex<Option<Teardown>>,
}

impl PeerSession {
    /// Every request goes through here, so the timeout cannot be forgotten at
    /// one call site.
    async fn bounded<F, T>(what: &str, future: F) -> Result<T>
    where
        F: std::future::Future<Output = std::result::Result<T, rmcp::service::ServiceError>>,
    {
        match tokio::time::timeout(REQUEST_TIMEOUT, future).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(AppError::new(
                ErrorCode::InternalError,
                format!("MCP {what} failed: {err}"),
            )),
            Err(_) => Err(AppError::new(
                ErrorCode::InternalError,
                format!("MCP {what} timed out after {}s", REQUEST_TIMEOUT.as_secs()),
            )),
        }
    }
}

#[async_trait]
impl Session for PeerSession {
    async fn list_tools(&self) -> Result<Vec<ToolDef>> {
        let result = Self::bounded("tools/list", self.peer.list_all_tools()).await?;
        Ok(result
            .into_iter()
            .map(|tool| ToolDef {
                name: tool.name.to_string(),
                description: tool.description.map(|d| d.to_string()),
                input_schema: (*tool.input_schema).clone(),
            })
            .collect())
    }

    async fn list_prompts(&self) -> Result<Vec<PromptDef>> {
        let result = Self::bounded("prompts/list", self.peer.list_all_prompts()).await?;
        Ok(result
            .into_iter()
            .map(|prompt| PromptDef {
                name: prompt.name,
                description: prompt.description,
                arguments: prompt
                    .arguments
                    .unwrap_or_default()
                    .into_iter()
                    .map(|arg| McpPromptArg {
                        name: arg.name,
                        description: arg.description,
                        required: arg.required,
                    })
                    .collect(),
            })
            .collect())
    }

    async fn list_resources(&self) -> Result<Vec<ResourceDef>> {
        let result = Self::bounded("resources/list", self.peer.list_all_resources()).await?;
        Ok(result
            .into_iter()
            .map(|resource| ResourceDef {
                uri: resource.uri,
                name: resource.name,
                description: resource.description,
                mime_type: resource.mime_type,
            })
            .collect())
    }

    async fn call_tool(&self, name: &str, args: Map<String, Value>) -> Result<Value> {
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(args);
        let result = Self::bounded("tools/call", self.peer.call_tool(params)).await?;
        serde_json::to_value(result).map_err(|err| {
            AppError::new(
                ErrorCode::InternalError,
                format!("MCP tool result was not serialisable: {err}"),
            )
        })
    }

    async fn get_prompt(
        &self,
        name: &str,
        args: BTreeMap<String, String>,
    ) -> Result<Vec<PromptMessage>> {
        let arguments = if args.is_empty() {
            None
        } else {
            Some(
                args.into_iter()
                    .map(|(k, v)| (k, Value::String(v)))
                    .collect::<Map<String, Value>>(),
            )
        };
        let mut params = GetPromptRequestParams::new(name.to_string());
        params.arguments = arguments;
        let result = Self::bounded("prompts/get", self.peer.get_prompt(params)).await?;
        Ok(result
            .messages
            .into_iter()
            .map(|message| PromptMessage {
                text: message
                    .content
                    .as_text()
                    .map(|t| t.text.clone())
                    .unwrap_or_default(),
            })
            .collect())
    }

    async fn read_resource(&self, uri: &str) -> Result<Vec<ResourceChunk>> {
        let params = ReadResourceRequestParams::new(uri.to_string());
        let result = Self::bounded("resources/read", self.peer.read_resource(params)).await?;
        Ok(result
            .contents
            .into_iter()
            .map(|content| match content {
                rmcp::model::ResourceContents::TextResourceContents { text, .. } => ResourceChunk {
                    text: Some(text),
                    is_blob: false,
                },
                // A blob, or a content kind a later protocol version adds:
                // either way there is no text to inject, and saying so is
                // better than inlining base64 into a prompt.
                _ => ResourceChunk {
                    text: None,
                    is_blob: true,
                },
            })
            .collect())
    }

    fn close(&self) {
        let taken = self.cancel.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(token) = taken {
            token.cancel();
        }
        let teardown = self
            .teardown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(teardown) = teardown {
            teardown();
        }
    }
}
