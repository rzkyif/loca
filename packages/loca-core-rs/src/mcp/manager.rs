//! One live connection per enabled server.
//!
//! The manager owns no persistence. It reconciles the live set against the
//! configured one, caches each server's capability lists in memory, and exposes
//! the three request round-trips. Whoever holds both this and the registry
//! merges them into the wire shape.
//!
//! Four behaviours here exist for a stated reason and each is load-bearing:
//!
//! 1. **[`Manager::sync`] serialises its reconciliation, but not its
//!    connects.** Two overlapping CRUD requests must not both pass the "is it
//!    connected?" check and start the same server twice, leaving the second to
//!    replace the first while its child process keeps running; the claim in
//!    `connecting` is what prevents that. The lock is released before the
//!    connects because a connect can take up to `CONNECT_TIMEOUT`, and holding
//!    it that long would queue every CRUD route behind the very server the user
//!    is trying to remove.
//! 2. **A disconnect removes the connection before closing it**, so the close
//!    that follows is seen as intentional rather than as a drop that should
//!    trigger a reconnect. The generation counter is what makes "is this still
//!    the active connection?" answerable.
//! 3. **An unexpected drop reconnects on a bounded backoff**, reconciling
//!    against the CURRENT desired set rather than the one that was live when
//!    the timer was armed, and it is cancelled the moment the server is
//!    disabled. Reconnecting re-establishes a connection the user already
//!    enabled; it never reaches a new endpoint (D-032).
//! 4. **A `list_changed` notification from a connection that is no longer
//!    active is ignored**, same generation check as (2).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::contract::api::ServerToClientFrame;
use crate::error::{AppError, ErrorCode, Result};
use crate::transport::FrameBus;

use super::registry::ServerConfig;
use super::session::{
    Capability, Connector, PromptDef, PromptMessage, ResourceChunk, ResourceDef, Session,
    SessionEvent, ToolDef,
};
use crate::contract::api::McpConnectionStatus;

/// The outside bound on ONE server's connect, covering everything the transport
/// does before there is a session: DNS, TCP, TLS, OAuth metadata discovery, and
/// the `initialize` handshake.
///
/// The handshake has its own, tighter bound
/// ([`session::HANDSHAKE_TIMEOUT`](super::session::HANDSHAKE_TIMEOUT), 60s,
/// which is the specification's), so in the ordinary black-hole case the user
/// gets that message rather than this one. This exists for the steps that sit
/// OUTSIDE the handshake future, above all the OAuth discovery and dynamic
/// registration that `AuthorizationManager::new` performs against an
/// authorization server before any MCP frame is sent.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);

/// Backoff steps for auto-reconnecting an enabled server after an unexpected
/// drop. After the last step the manager gives up and leaves the server in
/// `error` until the user presses Reconnect or a CRUD change resyncs.
const RECONNECT_BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
];

/// What a server told us it can do, as of the last fetch.
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    pub tools: Vec<ToolDef>,
    pub prompts: Vec<PromptDef>,
    pub resources: Vec<ResourceDef>,
}

/// The live status of one server.
#[derive(Clone, Debug)]
pub struct LiveStatus {
    pub status: McpConnectionStatus,
    pub error: Option<String>,
}

struct Connection {
    /// Which incarnation this is. A close or a notification carrying an older
    /// generation belongs to a connection that has already been replaced.
    generation: u64,
    session: Arc<dyn Session>,
    caps: Capabilities,
}

struct Inner {
    conns: Mutex<HashMap<String, Connection>>,
    /// Servers that have no live connection and a reason worth showing.
    failed: Mutex<HashMap<String, String>>,
    /// Connects in flight, so `status` can report `connecting`.
    connecting: Mutex<HashSet<String>>,
    /// The enabled set from the most recent sync, so a drop can reconcile
    /// against current config without reaching back into the registry.
    desired: Mutex<Vec<ServerConfig>>,
    /// The armed backoff timer per server, if any.
    reconnects: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// How far into [`RECONNECT_BACKOFF`] each server has got. Deliberately NOT
    /// stored with the timer: cancelling a timer must not forget how many
    /// attempts have been made, or every step would be the first one and the
    /// give-up branch would be unreachable. Cleared by a successful connect and
    /// by an intentional disconnect, which are the two things that end an
    /// incident.
    reconnect_attempts: Mutex<HashMap<String, usize>>,
    generation: AtomicU64,
    /// Serialises reconciliation. Held across `reconcile` and released before
    /// the connects it returns, which is why it is a tokio mutex rather than a
    /// std one: `reconcile` awaits the disconnects it decides on.
    sync_lock: tokio::sync::Mutex<()>,
    connector: Arc<dyn Connector>,
    frames: Arc<FrameBus>,
}

#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

impl Manager {
    pub fn new(connector: Arc<dyn Connector>, frames: Arc<FrameBus>) -> Self {
        Self {
            inner: Arc::new(Inner {
                conns: Mutex::new(HashMap::new()),
                failed: Mutex::new(HashMap::new()),
                connecting: Mutex::new(HashSet::new()),
                desired: Mutex::new(Vec::new()),
                reconnects: Mutex::new(HashMap::new()),
                reconnect_attempts: Mutex::new(HashMap::new()),
                generation: AtomicU64::new(0),
                sync_lock: tokio::sync::Mutex::new(()),
                connector,
                frames,
            }),
        }
    }

    pub fn stdio_supported(&self) -> bool {
        self.inner.connector.stdio_supported()
    }

    pub fn status(&self, id: &str) -> LiveStatus {
        if lock(&self.inner.conns).contains_key(id) {
            return LiveStatus {
                status: McpConnectionStatus::Connected,
                error: None,
            };
        }
        if lock(&self.inner.connecting).contains(id) {
            return LiveStatus {
                status: McpConnectionStatus::Connecting,
                error: None,
            };
        }
        if let Some(error) = lock(&self.inner.failed).get(id) {
            return LiveStatus {
                status: McpConnectionStatus::Error,
                error: Some(error.clone()),
            };
        }
        LiveStatus {
            status: McpConnectionStatus::Disconnected,
            error: None,
        }
    }

    pub fn capabilities(&self, id: &str) -> Capabilities {
        lock(&self.inner.conns)
            .get(id)
            .map(|c| c.caps.clone())
            .unwrap_or_default()
    }

    /// Connect every enabled server that is not connected, and disconnect any
    /// connection whose server is now disabled or gone.
    ///
    /// Each connect is independent, so one server failing to start never blocks
    /// the others; the failure is remembered as an `error` status with its
    /// message.
    pub async fn sync(&self, servers: Vec<ServerConfig>) {
        // The lock covers the RECONCILIATION only, and is released before the
        // connects. Held across them, one server at an endpoint that accepts
        // TCP and never answers would hold it for the whole of
        // `CONNECT_TIMEOUT`, and every route that resyncs (create, patch,
        // delete, reconnect) would queue behind it: the user could not remove
        // the server that was wedging them. Two overlapping syncs still cannot
        // start the same server twice, because `reconcile` claims each id in
        // `connecting` while it holds the lock.
        let to_start = {
            let _guard = self.inner.sync_lock.lock().await;
            self.reconcile(servers).await
        };
        // Concurrently: a server whose command takes ten seconds to start must
        // not delay every other server's connect behind it.
        futures_util::future::join_all(to_start.into_iter().map(|s| self.connect_one(s))).await;
    }

    /// Bring the live set in line with the desired one, and return the servers
    /// that still need connecting, each already claimed in `connecting`.
    async fn reconcile(&self, servers: Vec<ServerConfig>) -> Vec<ServerConfig> {
        let wanted: HashMap<String, ServerConfig> = servers
            .iter()
            .filter(|s| s.wants_connection())
            .map(|s| (s.id.clone(), s.clone()))
            .collect();
        *lock(&self.inner.desired) = servers;

        let stale: Vec<String> = lock(&self.inner.conns)
            .keys()
            .filter(|id| !wanted.contains_key(*id))
            .cloned()
            .collect();
        for id in stale {
            self.disconnect(&id).await;
        }
        lock(&self.inner.failed).retain(|id, _| wanted.contains_key(id));
        let orphaned: Vec<String> = lock(&self.inner.reconnects)
            .keys()
            .filter(|id| !wanted.contains_key(*id))
            .cloned()
            .collect();
        for id in orphaned {
            self.cancel_reconnect(&id);
        }

        let conns = lock(&self.inner.conns);
        let mut connecting = lock(&self.inner.connecting);
        wanted
            .into_values()
            // `insert` returning false means a connect for that id is already
            // in flight from an earlier sync that has since released the lock.
            // Claiming here, under the lock, is what replaces holding the lock
            // across the connect itself.
            .filter(|s| !conns.contains_key(&s.id) && connecting.insert(s.id.clone()))
            .collect()
    }

    /// Open one connection, fetch its capabilities, and start watching it.
    /// Never fails outward: a failure becomes the server's `error` status.
    ///
    /// The id is already claimed in `connecting` by [`Manager::reconcile`]; this
    /// releases the claim when it is done, whichever way it went.
    async fn connect_one(&self, server: ServerConfig) {
        lock(&self.inner.failed).remove(&server.id);

        let result = self.open(&server).await;

        lock(&self.inner.connecting).remove(&server.id);
        // The server may have been disabled or deleted while this connect was
        // in flight, which is possible only because the sync lock is not held
        // here. A sync that ran in that window could not have disconnected it,
        // because it was not yet in `conns` to be found, so this is the only
        // place that connection is dropped. `desired` is written under the sync
        // lock and read here AFTER `open` has published the connection, so
        // either that sync saw the connection or this read sees the new desired
        // set; there is no interleaving where both miss it.
        if !self.still_wanted(&server.id) {
            self.disconnect(&server.id).await;
            return;
        }
        match result {
            Ok(caps_count) => {
                tracing::info!(
                    target: "loca::mcp",
                    "connected {}: {} tools, {} prompts, {} resources",
                    server.name,
                    caps_count.0,
                    caps_count.1,
                    caps_count.2,
                );
            }
            Err(err) => {
                lock(&self.inner.failed).insert(server.id.clone(), err.message.clone());
                tracing::warn!(
                    target: "loca::mcp",
                    "connect {} failed: {}",
                    server.name,
                    err.message,
                );
            }
        }
    }

    /// Whether the most recent sync still wants this server connected.
    fn still_wanted(&self, id: &str) -> bool {
        lock(&self.inner.desired)
            .iter()
            .any(|s| s.id == id && s.wants_connection())
    }

    async fn open(&self, server: &ServerConfig) -> Result<(usize, usize, usize)> {
        // Bounded, because everything before the session exists is outside
        // `PeerSession::bounded`. Without it a connector that never resolves
        // leaves this server `connecting` forever and its claim in `connecting`
        // never released, so no later sync would retry it either.
        let connected =
            match tokio::time::timeout(CONNECT_TIMEOUT, self.inner.connector.connect(server)).await
            {
                Ok(connected) => connected?,
                Err(_) => {
                    return Err(AppError::new(
                        ErrorCode::ServerUnavailable,
                        format!("Connecting timed out after {}s.", CONNECT_TIMEOUT.as_secs()),
                    ))
                }
            };
        let session = connected.session;
        // A server may implement only some of the list methods, so each list is
        // fetched defensively and an error reads as "none of those".
        let caps = Capabilities {
            tools: session.list_tools().await.unwrap_or_default(),
            prompts: session.list_prompts().await.unwrap_or_default(),
            resources: session.list_resources().await.unwrap_or_default(),
        };
        let counts = (caps.tools.len(), caps.prompts.len(), caps.resources.len());
        let generation = self.inner.generation.fetch_add(1, Ordering::Relaxed) + 1;
        lock(&self.inner.conns).insert(
            server.id.clone(),
            Connection {
                generation,
                session,
                caps,
            },
        );
        lock(&self.inner.reconnects).remove(&server.id);
        // The incident is over: a later drop starts the schedule from the top.
        lock(&self.inner.reconnect_attempts).remove(&server.id);
        self.watch(server.id.clone(), generation, connected.events);
        Ok(counts)
    }

    /// Drop a server's live session.
    ///
    /// Deliberately removes the connection BEFORE closing it: the watcher then
    /// sees a generation that is no longer active and treats the close as
    /// intentional rather than as a drop to reconnect from. An intentional
    /// disconnect also cancels any pending backoff and resets the attempt
    /// count, so a later re-enable starts fresh.
    pub async fn disconnect(&self, id: &str) {
        self.cancel_reconnect(id);
        lock(&self.inner.reconnect_attempts).remove(id);
        let removed = lock(&self.inner.conns).remove(id);
        if let Some(conn) = removed {
            conn.session.close();
        }
    }

    /// React to whatever a connection reports on its own.
    fn watch(
        &self,
        id: String,
        generation: u64,
        mut events: tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
    ) {
        let manager = self.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if !manager.is_active(&id, generation) {
                    return;
                }
                match event {
                    SessionEvent::ListChanged(which) => {
                        manager.refresh(&id, generation, which).await;
                    }
                    SessionEvent::Closed => {
                        manager.on_dropped(&id);
                        return;
                    }
                }
            }
        });
    }

    fn is_active(&self, id: &str, generation: u64) -> bool {
        lock(&self.inner.conns)
            .get(id)
            .is_some_and(|c| c.generation == generation)
    }

    /// Re-fetch one capability list after a server's `list_changed`, then
    /// repaint.
    async fn refresh(&self, id: &str, generation: u64, which: Capability) {
        let Some(session) = self.session_if_active(id, generation) else {
            return;
        };
        let fetched = match which {
            Capability::Tools => session.list_tools().await.map(Fetched::Tools),
            Capability::Prompts => session.list_prompts().await.map(Fetched::Prompts),
            Capability::Resources => session.list_resources().await.map(Fetched::Resources),
        };
        let Ok(fetched) = fetched else {
            return;
        };
        {
            let mut conns = lock(&self.inner.conns);
            let Some(conn) = conns.get_mut(id) else {
                return;
            };
            if conn.generation != generation {
                return;
            }
            match fetched {
                Fetched::Tools(tools) => conn.caps.tools = tools,
                Fetched::Prompts(prompts) => conn.caps.prompts = prompts,
                Fetched::Resources(resources) => conn.caps.resources = resources,
            }
        }
        self.repaint();
    }

    /// An unexpected close: flip to `error`, repaint, and start the backoff.
    fn on_dropped(&self, id: &str) {
        lock(&self.inner.conns).remove(id);
        lock(&self.inner.failed).insert(id.to_string(), "Connection closed".to_string());
        self.repaint();
        self.schedule_reconnect(id.to_string());
    }

    /// Arm the next backoff step for a dropped enabled server. Stops once the
    /// steps are exhausted, leaving the server in `error` for a manual retry.
    fn schedule_reconnect(&self, id: String) {
        self.cancel_reconnect(&id);
        let attempt = lock(&self.inner.reconnect_attempts)
            .get(&id)
            .copied()
            .unwrap_or(0);
        let Some(delay) = RECONNECT_BACKOFF.get(attempt).copied() else {
            // The schedule is exhausted. The server stays in `error` until the
            // user presses Reconnect or a CRUD change resyncs it.
            return;
        };
        lock(&self.inner.reconnect_attempts).insert(id.clone(), attempt + 1);
        let manager = self.clone();
        let for_task = id.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let desired = lock(&manager.inner.desired).clone();
            // Reconciling through `sync` rather than connecting directly is
            // what makes a server disabled while the timer was running stay
            // disconnected.
            manager.sync(desired).await;
            let still_wanted = lock(&manager.inner.desired)
                .iter()
                .any(|s| s.id == for_task && s.wants_connection());
            // Bound the guard rather than reading it inside the condition: a
            // temporary from an `if` condition lives until the end of the whole
            // statement, so the body would run holding the connection map.
            let connected = lock(&manager.inner.conns).contains_key(&for_task);
            if still_wanted && !connected {
                manager.schedule_reconnect(for_task);
            }
        });
        lock(&self.inner.reconnects).insert(id, handle);
    }

    /// Disarm a pending backoff timer, leaving the attempt count alone: this is
    /// called both to end an incident and to re-arm within one.
    fn cancel_reconnect(&self, id: &str) {
        if let Some(handle) = lock(&self.inner.reconnects).remove(id) {
            handle.abort();
        }
    }

    fn session_if_active(&self, id: &str, generation: u64) -> Option<Arc<dyn Session>> {
        let conns = lock(&self.inner.conns);
        let conn = conns.get(id)?;
        (conn.generation == generation).then(|| Arc::clone(&conn.session))
    }

    /// The session for a request, or the error the TypeScript produces for the
    /// same request.
    ///
    /// `internal_error` reads oddly for "that server is not connected", and it
    /// is deliberate: the Deno core raises a bare `Error` here, which its
    /// handler renders as a 500, so a code of our own choosing would make this
    /// route permanently unassertable by the conformance suite while both cores
    /// run. The client catches any failure from a resolve and shows one notice
    /// either way (`UserInput.svelte:405`), so nothing observable turns on it.
    /// Worth tightening to `server_unavailable` at cutover, when the oracle
    /// retires.
    fn session_or_err(&self, id: &str) -> Result<Arc<dyn Session>> {
        lock(&self.inner.conns)
            .get(id)
            .map(|c| Arc::clone(&c.session))
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::InternalError,
                    format!("MCP server {id} is not connected"),
                )
            })
    }

    /// Tell every client to repaint. Called after a change the manager made on
    /// its own; the routes broadcast their own CRUD changes.
    fn repaint(&self) {
        self.inner
            .frames
            .broadcast_all(&ServerToClientFrame::McpSnapshot);
    }

    pub async fn call_tool(
        &self,
        server_id: &str,
        name: &str,
        args: Map<String, Value>,
    ) -> Result<Value> {
        self.session_or_err(server_id)?.call_tool(name, args).await
    }

    pub async fn get_prompt(
        &self,
        server_id: &str,
        name: &str,
        args: std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<PromptMessage>> {
        self.session_or_err(server_id)?.get_prompt(name, args).await
    }

    pub async fn read_resource(&self, server_id: &str, uri: &str) -> Result<Vec<ResourceChunk>> {
        self.session_or_err(server_id)?.read_resource(uri).await
    }

    /// Cancel every pending reconnect and close every connection, so a spawned
    /// stdio child is terminated rather than orphaned on exit.
    pub async fn shutdown(&self) {
        let pending: Vec<String> = lock(&self.inner.reconnects).keys().cloned().collect();
        for id in pending {
            self.cancel_reconnect(&id);
        }
        let ids: Vec<String> = lock(&self.inner.conns).keys().cloned().collect();
        for id in ids {
            self.disconnect(&id).await;
        }
    }
}

enum Fetched {
    Tools(Vec<ToolDef>),
    Prompts(Vec<PromptDef>),
    Resources(Vec<ResourceDef>),
}

/// A poisoned lock here means another thread panicked while holding it. The
/// state behind it is a map of live connections rather than an invariant that
/// can be half-updated, so recovering is strictly better than propagating a
/// panic through every route.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
