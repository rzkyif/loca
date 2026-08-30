//! MCP server connections: stdio, remote HTTP, and OAuth token resolution.
//!
//! Enabling a server is the consent gate: an stdio server runs an arbitrary
//! local command outside the tool sandbox and a remote server is an arbitrary
//! endpoint, so servers are created disabled and per-tool/per-prompt toggles
//! default off. Every request here is user-initiated; the only non-interactive
//! paths ([`Mcp::start`] and the reconnect backoff) re-establish a connection
//! the user already enabled and never reach an endpoint they did not name.

pub mod connector;
pub mod manager;
pub mod oauth;
pub mod registry;
pub mod remote;
pub mod routes;
pub mod secret_key;
pub mod session;
pub mod stdio;
pub mod tokens;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::contract::api::{
    McpPrompt, McpResource, McpServer, McpTransportKind, ServerToClientFrame, Tool,
    ToolProviderKind,
};
use crate::db::Db;
use crate::error::Result;
use crate::paths::Paths;
use crate::secrets::Vault;
use crate::sidecars::Manager as Sidecars;
use crate::transport::FrameBus;

use connector::LiveConnector;
use manager::Manager;
use oauth::flow::{Flows, OnComplete};
use registry::{NewServer, Registry, ServerConfig, ServerPatch, ToggleSet};
use session::Connector;
use stdio::StdioSupport;
use tokens::{ResolvableResource, ResourceReader, TokenResult};

/// The MCP subsystem: configured servers, live connections, and sign-in flows.
/// Exactly one per process: it owns the connection map, reconnect timers and
/// pending OAuth listeners.
#[derive(Clone)]
pub struct Mcp {
    inner: Arc<Inner>,
}

struct Inner {
    registry: Registry,
    manager: Manager,
    flows: Flows,
    vault: Vault,
    frames: Arc<FrameBus>,
}

impl Mcp {
    pub fn new(
        paths: Arc<Paths>,
        db: Db,
        vault: Vault,
        frames: Arc<FrameBus>,
        sidecars: Arc<Sidecars>,
        stdio: StdioSupport,
    ) -> Self {
        let connector = Arc::new(LiveConnector::new(paths, vault.clone(), sidecars, stdio));
        Self::with_connector(db, vault, frames, connector)
    }

    /// The test seam: connections opened by something other than a subprocess
    /// or a URL.
    pub fn with_connector(
        db: Db,
        vault: Vault,
        frames: Arc<FrameBus>,
        connector: Arc<dyn Connector>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                registry: Registry::new(db),
                manager: Manager::new(connector, Arc::clone(&frames)),
                flows: Flows::new(),
                vault,
                frames,
            }),
        }
    }

    /// Whether this device can run a local MCP server (`capabilities.stdioMcp`).
    pub fn stdio_supported(&self) -> bool {
        self.inner.manager.stdio_supported()
    }

    /// Connect to the servers the user has already enabled, in the background.
    pub fn start(&self) {
        let mcp = self.clone();
        tokio::spawn(async move {
            if let Err(err) = mcp.resync().await {
                tracing::warn!(target: "loca::mcp", "MCP sync failed: {err}");
            }
        });
    }

    /// Cancel every pending reconnect and sign-in, and close every connection.
    ///
    /// Does not wait for stdio children: closing a session hands the child to
    /// `stdio::terminate`, which runs SIGTERM-then-SIGKILL in its own task. The
    /// sidecar manager stops the same children right after, so nothing leaks.
    pub async fn shutdown(&self) {
        self.inner.flows.cancel_all();
        self.inner.manager.shutdown().await;
    }

    // --- projections ---------------------------------------------------------

    pub async fn list(&self) -> Result<Vec<McpServer>> {
        Ok(self
            .inner
            .registry
            .list()
            .await?
            .into_iter()
            .map(|config| self.project(config))
            .collect())
    }

    pub async fn get(&self, id: &str) -> Result<McpServer> {
        let config = self.inner.registry.get_or_err(id).await?;
        Ok(self.project(config))
    }

    /// Merge a stored row with the live status and capability counts.
    fn project(&self, config: ServerConfig) -> McpServer {
        let live = self.inner.manager.status(&config.id);
        let caps = self.inner.manager.capabilities(&config.id);
        McpServer {
            id: config.id,
            name: config.name,
            kind: config.kind,
            command: config.command,
            args: config.args,
            runtime: config.runtime,
            deno_allow_all: config.deno_allow_all,
            deno_permissions: config.deno_permissions,
            url: config.url,
            remote_auth: config.remote_auth,
            has_auth: config.has_auth,
            oauth_authorized: config.oauth_authorized,
            enabled: config.enabled,
            tool_enabled: config.tool_enabled,
            prompt_enabled: config.prompt_enabled,
            tool_always_available_off: config.tool_always_available_off,
            status: live.status,
            status_error: live.error,
            tool_count: caps.tools.len() as i64,
            prompt_count: caps.prompts.len() as i64,
            resource_count: caps.resources.len() as i64,
            created_at_ms: config.created_at_ms,
            updated_at_ms: config.updated_at_ms,
        }
    }

    /// Every tool from every connected server, in the shared `Tool` shape.
    pub async fn list_all_tools(&self) -> Result<Vec<Tool>> {
        let mut out = Vec::new();
        for server in self.inner.registry.list().await? {
            for tool in self.inner.manager.capabilities(&server.id).tools {
                out.push(Tool {
                    id: format!("{}::{}", server.id, tool.name),
                    extension_id: server.id.clone(),
                    provider_kind: ToolProviderKind::Mcp,
                    provider_name: Some(server.name.clone()),
                    description: tool.description.unwrap_or_default(),
                    parameters: tool.input_schema,
                    triggers: Vec::new(),
                    fn_export: String::new(),
                    // The stored set is an opt-OUT: MCP tools default to
                    // always-available.
                    always_available: !server.tool_always_available_off.contains(&tool.name),
                    platforms: Vec::new(),
                    enabled: server.tool_enabled.contains(&tool.name),
                    required_permissions: Vec::new(),
                    missing_required: Vec::new(),
                    grants: Vec::new(),
                    name: tool.name,
                });
            }
        }
        Ok(out)
    }

    /// Every prompt from every connected server, with the user's enable flag.
    pub async fn list_prompts(&self) -> Result<Vec<McpPrompt>> {
        let mut out = Vec::new();
        for server in self.inner.registry.list().await? {
            for prompt in self.inner.manager.capabilities(&server.id).prompts {
                out.push(McpPrompt {
                    server_id: server.id.clone(),
                    server_name: server.name.clone(),
                    enabled: server.prompt_enabled.contains(&prompt.name),
                    name: prompt.name,
                    description: prompt.description,
                    arguments: prompt.arguments,
                });
            }
        }
        Ok(out)
    }

    /// Every resource from every connected server; there is no per-resource toggle.
    pub async fn list_resources(&self) -> Result<Vec<McpResource>> {
        let mut out = Vec::new();
        for server in self.inner.registry.list().await? {
            for resource in self.inner.manager.capabilities(&server.id).resources {
                out.push(McpResource {
                    server_id: server.id.clone(),
                    server_name: server.name.clone(),
                    uri: resource.uri,
                    name: resource.name,
                    description: resource.description,
                    mime_type: resource.mime_type,
                });
            }
        }
        Ok(out)
    }

    // --- mutation ------------------------------------------------------------

    pub async fn create(&self, input: NewServer) -> Result<McpServer> {
        let config = self.inner.registry.create(input).await?;
        Ok(self.project(config))
    }

    pub async fn update(&self, id: &str, patch: ServerPatch) -> Result<McpServer> {
        let config = self.inner.registry.update(id, patch).await?;
        Ok(self.project(config))
    }

    /// Delete the row, its secrets, and any sign-in waiting on a browser for it.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.inner.flows.cancel(id);
        self.inner.registry.delete(id).await?;
        self.forget_secrets(id).await;
        Ok(())
    }

    pub async fn set_tool_enabled(&self, id: &str, tool: &str, enabled: bool) -> Result<McpServer> {
        let config = self
            .inner
            .registry
            .toggle(id, ToggleSet::Tool, tool, enabled)
            .await?;
        Ok(self.project(config))
    }

    pub async fn set_prompt_enabled(
        &self,
        id: &str,
        prompt: &str,
        enabled: bool,
    ) -> Result<McpServer> {
        let config = self
            .inner
            .registry
            .toggle(id, ToggleSet::Prompt, prompt, enabled)
            .await?;
        Ok(self.project(config))
    }

    /// Always-available is stored as an opt-out set, so OFF adds the name.
    pub async fn set_tool_always_available(
        &self,
        id: &str,
        tool: &str,
        always_available: bool,
    ) -> Result<McpServer> {
        let config = self
            .inner
            .registry
            .toggle(
                id,
                ToggleSet::ToolAlwaysAvailableOff,
                tool,
                !always_available,
            )
            .await?;
        Ok(self.project(config))
    }

    // --- secrets -------------------------------------------------------------

    /// `None` leaves the stored bearer token untouched, an empty string clears
    /// it, a value replaces it. Returns the new `has_auth`, or `None` if unchanged.
    pub async fn apply_auth_token(&self, id: &str, token: Option<&str>) -> Result<Option<bool>> {
        let name = secret_key::auth_secret_name(id);
        match token {
            None => Ok(None),
            Some("") => {
                self.inner.vault.delete(&name).await?;
                Ok(Some(false))
            }
            Some(value) => {
                self.inner.vault.set(&name, value).await?;
                Ok(Some(true))
            }
        }
    }

    /// Best effort: a vault that cannot be reached must not leave the row behind.
    async fn forget_secrets(&self, id: &str) {
        for name in [
            secret_key::auth_secret_name(id),
            secret_key::oauth_secret_name(id),
        ] {
            if let Err(err) = self.inner.vault.delete(&name).await {
                tracing::warn!(target: "loca::mcp", "could not clear an MCP secret: {err}");
            }
        }
    }

    // --- connections ---------------------------------------------------------

    /// Reconcile live connections to the current enabled set, then repaint.
    pub async fn resync(&self) -> Result<()> {
        let servers = self.inner.registry.list().await?;
        self.inner.manager.sync(servers).await;
        self.broadcast();
        Ok(())
    }

    /// Drop a server's live session so a following resync reconnects it.
    pub async fn disconnect(&self, id: &str) {
        self.inner.manager.disconnect(id).await;
    }

    /// Tell every client to repaint the MCP screens.
    pub fn broadcast(&self) {
        self.inner
            .frames
            .broadcast_all(&ServerToClientFrame::McpSnapshot);
    }

    /// Fetch a prompt and flatten its messages into one instruction string.
    pub async fn resolve_prompt(
        &self,
        id: &str,
        prompt: &str,
        args: BTreeMap<String, String>,
    ) -> Result<String> {
        let messages = self.inner.manager.get_prompt(id, prompt, args).await?;
        Ok(tokens::flatten_prompt_messages(&messages))
    }

    /// Call a tool on a connected server. Dropping the future cancels the wait.
    pub async fn call_tool(
        &self,
        server_id: &str,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value> {
        self.inner.manager.call_tool(server_id, name, args).await
    }

    /// Expand every `@resource` in a turn's user text into reference blocks.
    pub async fn resolve_tokens(&self, text: &str) -> TokenResult {
        let resources = match self.list_resources().await {
            Ok(resources) => resources,
            Err(err) => {
                tracing::warn!(target: "loca::mcp", "could not list MCP resources: {err}");
                return TokenResult::default();
            }
        };
        let resolvable = resources
            .into_iter()
            .map(|r| ResolvableResource {
                server_id: r.server_id,
                server_name: r.server_name,
                uri: r.uri,
                name: r.name,
            })
            .collect();
        tokens::resolve(text, resolvable, self).await
    }

    // --- oauth ---------------------------------------------------------------

    /// Begin the sign-in for a remote server and hand back the URL to open, or
    /// `None` when the stored tokens already work.
    pub async fn start_oauth(&self, id: &str, url: &str) -> Result<Option<String>> {
        let mcp = self.clone();
        let server_id = id.to_string();
        // Runs when the browser comes back, long after this request answered.
        let on_complete: OnComplete = Box::new(move |ok| {
            if !ok {
                return;
            }
            tokio::spawn(async move {
                let patch = ServerPatch {
                    remote_auth: Some(crate::contract::api::McpRemoteAuth::Oauth),
                    oauth_authorized: Some(true),
                    ..ServerPatch::default()
                };
                if let Err(err) = mcp.inner.registry.update(&server_id, patch).await {
                    tracing::warn!(
                        target: "loca::mcp",
                        "could not mark {server_id} authorized: {err}",
                    );
                    return;
                }
                if let Err(err) = mcp.resync().await {
                    tracing::warn!(target: "loca::mcp", "resync after sign-in failed: {err}");
                }
            });
        });
        self.inner
            .flows
            .start(id, url, &self.inner.vault, on_complete)
            .await
    }

    #[cfg(test)]
    pub(crate) fn vault_for_test(&self) -> &Vault {
        &self.inner.vault
    }

    pub fn cancel_oauth(&self, id: &str) {
        self.inner.flows.cancel(id);
    }

    /// A sign-in needs a remote server with a URL to discover metadata from.
    pub fn can_sign_in(server: &McpServer) -> bool {
        server.kind == McpTransportKind::Remote
            && server
                .url
                .as_deref()
                .map(str::trim)
                .is_some_and(|url| !url.is_empty())
    }
}

#[async_trait::async_trait]
impl ResourceReader for Mcp {
    async fn read(&self, server_id: &str, uri: &str) -> Result<String> {
        let chunks = self.inner.manager.read_resource(server_id, uri).await?;
        Ok(chunks
            .into_iter()
            .map(|chunk| match chunk.text {
                Some(text) => text,
                None if chunk.is_blob => "[binary content omitted]".to_string(),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// Declared by the consumer so nothing under `extensions/` imports an `mcp::` type.
#[async_trait::async_trait]
impl crate::extensions::McpTools for Mcp {
    async fn list_all_tools(&self) -> Result<Vec<Tool>> {
        Mcp::list_all_tools(self).await
    }
}
