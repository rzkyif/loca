//! One live MCP conversation, and the seam that opens it.
//!
//! Cancellation is dropping the future: no request takes an abort signal, a
//! caller selects over its own cancellation. Every request also carries
//! [`REQUEST_TIMEOUT`] so a hung server cannot stall a turn.

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

/// Bound on a single MCP request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on the `initialize` exchange, which cannot go through
/// [`PeerSession::bounded`] because there is no peer until it finishes. Nothing
/// underneath supplies one: rmcp's `expect_response` and the `reqwest::Client`
/// both have no timeout.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

/// What a bounded handshake produced.
pub enum Handshake<T, E> {
    Ready(T),
    Failed(E),
    TimedOut,
}

/// Run a transport's handshake under a bound (a parameter so a test can drive
/// a black-holed socket to the timeout quickly).
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

/// Distinguishes "did not answer" from "refused", which need different fixes.
pub fn handshake_timeout_message(timeout: Duration) -> String {
    format!(
        "The server did not finish the MCP handshake within {}s.",
        timeout.as_secs()
    )
}

#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub struct PromptDef {
    pub name: String,
    pub description: Option<String>,
    pub arguments: Vec<McpPromptArg>,
}

#[derive(Clone, Debug)]
pub struct ResourceDef {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

/// One piece of a resource read. A blob carries no text.
#[derive(Clone, Debug)]
pub struct ResourceChunk {
    pub text: Option<String>,
    pub is_blob: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Tools,
    Prompts,
    Resources,
}

/// Something a connection reported on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    ListChanged(Capability),
    /// The service loop ended; the manager decides whether it was intentional.
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
    /// Idempotent and best effort.
    fn close(&self);
}

#[derive(Clone, Debug)]
pub struct PromptMessage {
    pub text: String,
}

/// A freshly opened connection: the session plus its unsolicited events.
pub struct Connected {
    pub session: Arc<dyn Session>,
    pub events: mpsc::UnboundedReceiver<SessionEvent>,
}

/// How the manager opens a connection.
#[async_trait]
pub trait Connector: Send + Sync {
    async fn connect(&self, server: &ServerConfig) -> Result<Connected>;
    /// Mobile cannot run a subprocess, so an stdio server is refused before
    /// anything is spawned.
    fn stdio_supported(&self) -> bool;
}

/// What Core calls itself during the MCP handshake.
pub fn client_info() -> ClientInfo {
    // `ClientInfo` is `#[non_exhaustive]`.
    let mut info = ClientInfo::default();
    info.client_info = Implementation::new("Loca", CORE_VERSION);
    info
}

/// The `ClientHandler` half: turns `list_changed` notifications into events.
#[derive(Clone)]
pub struct EventHandler {
    tx: mpsc::UnboundedSender<SessionEvent>,
}

impl EventHandler {
    fn send(&self, event: SessionEvent) {
        // A send error means the connection is already being torn down.
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

/// Wrap a started service as a [`Session`]. The service is moved into a task
/// awaiting its termination (`waiting()` consumes it), which is what turns a
/// dropped connection into [`SessionEvent::Closed`].
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

/// Run once after the protocol close. A stdio connection passes the child
/// handle in here so the process lives exactly as long as the session.
pub type Teardown = Box<dyn FnOnce() + Send + Sync>;

/// Build the handler and the channel the manager reads.
pub fn event_channel() -> (EventHandler, mpsc::UnboundedReceiver<SessionEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (EventHandler { tx: tx.clone() }, rx)
}

/// The sender half again, for [`adopt`].
pub fn sender(handler: &EventHandler) -> mpsc::UnboundedSender<SessionEvent> {
    handler.tx.clone()
}

struct PeerSession {
    peer: Peer<RoleClient>,
    /// Taken by the first `close`, so cancelling twice is not an error.
    cancel: std::sync::Mutex<Option<rmcp::service::RunningServiceCancellationToken>>,
    teardown: std::sync::Mutex<Option<Teardown>>,
}

impl PeerSession {
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
                // A blob or a future content kind: no text to inject.
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
