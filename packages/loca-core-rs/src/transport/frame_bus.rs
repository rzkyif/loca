//! The frame bus: how a domain package pushes a frame to connected clients
//! without knowing a socket exists.
//!
//! A service calls [`FrameBus::broadcast_all`] or
//! [`FrameBus::broadcast_to_client`]. The bus renders the frame once and hands
//! the same bytes to every registered [`Connection`]. What a connection is
//! attached to is not the bus's concern: on desktop it is a WebSocket
//! (`super::ws`), and on mobile it is a stream of Tauri events.
//!
//! Inbound is the mirror image: a connection hands a decoded frame to the bus,
//! which routes it to the one registered [`InboundHandler`]. The transport
//! answers `ping` and swallows `pong`; the rest is the chat package's.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::mpsc;

use crate::contract::api::ServerToClientFrame;

/// One item on a connection's outbound queue.
#[derive(Clone, Debug)]
pub enum Outbound {
    /// A rendered frame. Shared, because one broadcast fans the same bytes out
    /// to every connection.
    Frame(Arc<str>),
    /// Close this connection with a WebSocket close code. A dropped sender
    /// cannot carry a code, and revocation has to arrive as 4001 rather than as
    /// an anonymous disconnect.
    Close { code: u16, reason: &'static str },
}

/// Receives client frames the transport did not handle itself.
///
/// Registered once, by the chat package. `kind` is pre-extracted and already
/// validated as a non-empty string; `frame` is the whole object, for the
/// handler to deserialize into its own `contract::inbound` type.
pub trait InboundHandler: Send + Sync {
    fn handle(&self, client_id: &str, kind: &str, frame: serde_json::Value);
}

/// Supplies the frames a just-connected client is seeded with.
///
/// Change broadcasts only fire on edges, so a client joining a steady core
/// would otherwise never learn the current state. Seeds go to the new
/// connection alone, never to the client's other connections.
pub trait ConnectionSeed: Send + Sync {
    fn seed(&self, client_id: &str) -> Vec<ServerToClientFrame>;
}

/// How many frames may queue on one connection before the bus starts warning.
///
/// Not a cap: dropping a frame silently is worse than the memory, and a socket
/// this far behind is about to be closed by `ws::WRITE_TIMEOUT`, which releases
/// the whole queue with the connection. That budget, not the pong deadline, is
/// what makes this a warning rather than a leak: a client that stops reading
/// blocks the write the pump is parked on, and the pong deadline is an arm of
/// the same `select!`, so it never gets polled to fire.
const QUEUE_WARN_DEPTH: usize = 4096;

struct Registered {
    id: u64,
    tx: mpsc::UnboundedSender<Outbound>,
    depth: Arc<AtomicU64>,
}

#[derive(Default)]
struct Registry {
    by_client: HashMap<String, Vec<Registered>>,
}

/// The per-core connection registry. Cheap to clone through the `Arc` the app
/// context holds it in.
#[derive(Default)]
pub struct FrameBus {
    registry: RwLock<Registry>,
    inbound: Mutex<Option<Arc<dyn InboundHandler>>>,
    seeds: Mutex<Vec<Arc<dyn ConnectionSeed>>>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for FrameBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FrameBus")
    }
}

impl FrameBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Route inbound frames to `handler`. Set once during boot; a frame that
    /// arrives before it is wired is warned about and dropped, which cannot
    /// happen in practice because no client can connect before boot finishes.
    pub fn on_inbound(&self, handler: Arc<dyn InboundHandler>) {
        *lock(&self.inbound) = Some(handler);
    }

    /// Whether an inbound handler is registered, for the boot-seam suite:
    /// losing boot's `on_inbound` call silently makes the whole chat module
    /// unreachable, since the hub then drops every `chat.start`.
    pub fn has_inbound(&self) -> bool {
        lock(&self.inbound).is_some()
    }

    /// Add a source of seed frames for new connections.
    pub fn add_seed(&self, seed: Arc<dyn ConnectionSeed>) {
        lock(&self.seeds).push(seed);
    }

    /// Attach a new connection for `client_id`.
    ///
    /// The seed frames are already queued on the returned connection, so any
    /// transport that drains it delivers them first with no ordering to get
    /// right per call site.
    pub fn register(self: &Arc<Self>, client_id: &str) -> Connection {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let depth = Arc::new(AtomicU64::new(0));
        self.registry
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .by_client
            .entry(client_id.to_string())
            .or_default()
            .push(Registered {
                id,
                tx: tx.clone(),
                depth: Arc::clone(&depth),
            });

        for seed in lock(&self.seeds).iter() {
            for frame in seed.seed(client_id) {
                if let Some(payload) = render(&frame) {
                    let _ = tx.send(Outbound::Frame(payload));
                    depth.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        tracing::debug!(target: "loca::ws", "client {client_id} connected (connection {id})");
        Connection {
            bus: Arc::clone(self),
            client_id: client_id.to_string(),
            id,
            rx,
            depth,
        }
    }

    /// Deliver to every connection of one client: an askUser request belonging
    /// to its chat, or its own per-client settings overlay.
    pub fn broadcast_to_client(&self, client_id: &str, frame: &ServerToClientFrame) {
        let Some(payload) = render(frame) else { return };
        let registry = self.registry.read().unwrap_or_else(|e| e.into_inner());
        if let Some(conns) = registry.by_client.get(client_id) {
            for conn in conns {
                send(conn, &payload);
            }
        }
    }

    /// Deliver to every connected client. Core-global state: download progress,
    /// core status, extension and MCP snapshots.
    pub fn broadcast_all(&self, frame: &ServerToClientFrame) {
        let Some(payload) = render(frame) else { return };
        let registry = self.registry.read().unwrap_or_else(|e| e.into_inner());
        for conns in registry.by_client.values() {
            for conn in conns {
                send(conn, &payload);
            }
        }
    }

    /// Force every live connection of one client closed, on revocation or a
    /// token rotation. A WS connection authenticates once, at upgrade, so
    /// without this an untrusted device keeps a working socket.
    pub fn close_client(&self, client_id: &str, code: u16, reason: &'static str) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        if let Some(conns) = registry.by_client.remove(client_id) {
            for conn in conns {
                let _ = conn.tx.send(Outbound::Close { code, reason });
            }
        }
    }

    /// Close every connection, for shutdown.
    pub fn shutdown(&self) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        for (_, conns) in registry.by_client.drain() {
            for conn in conns {
                let _ = conn.tx.send(Outbound::Close {
                    code: 1001,
                    reason: "core shutting down",
                });
            }
        }
    }

    /// Live connection count, for tests and diagnostics.
    pub fn connection_count(&self) -> usize {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .by_client
            .values()
            .map(|c| c.len())
            .sum()
    }

    fn deliver_inbound(&self, client_id: &str, kind: &str, frame: serde_json::Value) {
        let handler = lock(&self.inbound).clone();
        match handler {
            Some(h) => h.handle(client_id, kind, frame),
            None => tracing::warn!(
                target: "loca::ws",
                "dropped ws frame `{kind}`: no inbound handler is registered",
            ),
        }
    }

    fn deregister(&self, client_id: &str, id: u64) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        if let Some(conns) = registry.by_client.get_mut(client_id) {
            conns.retain(|c| c.id != id);
            if conns.is_empty() {
                registry.by_client.remove(client_id);
            }
        }
    }
}

fn send(conn: &Registered, payload: &Arc<str>) {
    if conn.tx.send(Outbound::Frame(Arc::clone(payload))).is_ok() {
        let depth = conn.depth.fetch_add(1, Ordering::Relaxed) + 1;
        if depth as usize == QUEUE_WARN_DEPTH {
            tracing::warn!(
                target: "loca::ws",
                "connection {} is {QUEUE_WARN_DEPTH} frames behind; it is not draining",
                conn.id,
            );
        }
    }
}

/// Render once per broadcast, not once per connection. A frame that will not
/// serialize is logged and dropped rather than taking the core down.
fn render(frame: &ServerToClientFrame) -> Option<Arc<str>> {
    match serde_json::to_string(frame) {
        Ok(json) => Some(Arc::from(json.as_str())),
        Err(err) => {
            tracing::error!(target: "loca::ws", "could not serialize an outbound frame: {err}");
            None
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One attached client connection. Dropping it deregisters, so a transport
/// task that returns early cannot leak a registry entry.
pub struct Connection {
    bus: Arc<FrameBus>,
    client_id: String,
    id: u64,
    rx: mpsc::UnboundedReceiver<Outbound>,
    depth: Arc<AtomicU64>,
}

impl Connection {
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// Next item to deliver, or `None` once the bus has dropped this
    /// connection.
    pub async fn next(&mut self) -> Option<Outbound> {
        let item = self.rx.recv().await;
        if item.is_some() {
            self.depth.fetch_sub(1, Ordering::Relaxed);
        }
        item
    }

    /// Hand a client frame to the registered inbound handler.
    pub fn deliver_inbound(&self, kind: &str, frame: serde_json::Value) {
        self.bus.deliver_inbound(&self.client_id, kind, frame);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.bus.deregister(&self.client_id, self.id);
        tracing::debug!(
            target: "loca::ws",
            "client {} disconnected (connection {})", self.client_id, self.id,
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contract::api::{CoreStatus, CoreStatusSnapshot};
    use std::sync::atomic::AtomicUsize;

    fn bus() -> Arc<FrameBus> {
        Arc::new(FrameBus::new())
    }

    fn frame(id: &str) -> ServerToClientFrame {
        ServerToClientFrame::UpdateStaged {
            version: id.to_string(),
        }
    }

    /// Take the next item from `conn`, bounded. A connection with nothing
    /// queued parks forever, so an unbounded drain turns a missing frame into a
    /// hang that wedges the whole `--lib` binary rather than reporting red.
    async fn next_item(conn: &mut Connection) -> Option<Outbound> {
        tokio::time::timeout(std::time::Duration::from_secs(5), conn.next())
            .await
            .unwrap_or_else(|_| panic!("no item arrived within 5s"))
    }

    fn version_of(item: Outbound) -> String {
        let Outbound::Frame(json) = item else {
            panic!("expected a frame, got {item:?}");
        };
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["version"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn a_frame_reaches_every_connection_of_the_addressed_client_only() {
        let bus = bus();
        let mut a1 = bus.register("a");
        let mut a2 = bus.register("a");
        let mut b = bus.register("b");

        bus.broadcast_to_client("a", &frame("for-a"));
        assert_eq!(version_of(next_item(&mut a1).await.unwrap()), "for-a");
        assert_eq!(version_of(next_item(&mut a2).await.unwrap()), "for-a");

        bus.broadcast_all(&frame("for-all"));
        assert_eq!(version_of(next_item(&mut a1).await.unwrap()), "for-all");
        assert_eq!(version_of(next_item(&mut a2).await.unwrap()), "for-all");
        // b saw the broadcast but never the client-addressed frame.
        assert_eq!(version_of(next_item(&mut b).await.unwrap()), "for-all");
    }

    #[tokio::test]
    async fn broadcasting_to_an_absent_client_is_a_no_op() {
        let bus = bus();
        bus.broadcast_to_client("nobody", &frame("x"));
        assert_eq!(bus.connection_count(), 0);
    }

    #[tokio::test]
    async fn every_connection_shares_one_rendering_of_the_frame() {
        let bus = bus();
        let mut a = bus.register("a");
        let mut b = bus.register("b");
        bus.broadcast_all(&frame("once"));
        let (Outbound::Frame(x), Outbound::Frame(y)) = (
            next_item(&mut a).await.unwrap(),
            next_item(&mut b).await.unwrap(),
        ) else {
            panic!("expected frames");
        };
        assert!(Arc::ptr_eq(&x, &y), "the frame was serialized twice");
    }

    #[tokio::test]
    async fn seeds_go_to_the_new_connection_and_nobody_else() {
        struct Seed;
        impl ConnectionSeed for Seed {
            fn seed(&self, client_id: &str) -> Vec<ServerToClientFrame> {
                vec![frame(&format!("seed-{client_id}"))]
            }
        }
        let bus = bus();
        bus.add_seed(Arc::new(Seed));

        let mut first = bus.register("a");
        assert_eq!(version_of(next_item(&mut first).await.unwrap()), "seed-a");

        // A second connection is seeded too; the first must not see it again.
        let mut second = bus.register("a");
        assert_eq!(version_of(next_item(&mut second).await.unwrap()), "seed-a");
        bus.broadcast_all(&frame("live"));
        assert_eq!(version_of(next_item(&mut first).await.unwrap()), "live");
    }

    #[tokio::test]
    async fn a_seed_is_queued_ahead_of_frames_sent_after_registration() {
        struct Seed;
        impl ConnectionSeed for Seed {
            fn seed(&self, _client_id: &str) -> Vec<ServerToClientFrame> {
                vec![ServerToClientFrame::CoreStatus {
                    snapshot: CoreStatusSnapshot {
                        status: CoreStatus::Idle,
                        detail: None,
                        progress: None,
                        queues: None,
                        subsystems: Vec::new(),
                    },
                }]
            }
        }
        let bus = bus();
        bus.add_seed(Arc::new(Seed));
        let mut conn = bus.register("a");
        bus.broadcast_all(&frame("after"));

        let Outbound::Frame(first) = next_item(&mut conn).await.unwrap() else {
            panic!("expected a frame")
        };
        assert!(first.contains("core.status"), "seed must arrive first");
        assert_eq!(version_of(next_item(&mut conn).await.unwrap()), "after");
    }

    #[tokio::test]
    async fn revocation_closes_with_a_code_the_client_can_read() {
        let bus = bus();
        let mut a = bus.register("a");
        let mut b = bus.register("b");
        bus.close_client("a", 4001, "client revoked");

        match next_item(&mut a).await.unwrap() {
            Outbound::Close { code, reason } => {
                assert_eq!(code, 4001);
                assert_eq!(reason, "client revoked");
            }
            other => panic!("expected a close, got {other:?}"),
        }
        // The revoked client is gone from the registry; the other is untouched.
        bus.broadcast_all(&frame("still-here"));
        assert_eq!(version_of(next_item(&mut b).await.unwrap()), "still-here");
        assert_eq!(bus.connection_count(), 1);
    }

    #[tokio::test]
    async fn shutdown_closes_everything() {
        let bus = bus();
        let mut a = bus.register("a");
        let mut b = bus.register("b");
        bus.shutdown();
        assert!(matches!(
            next_item(&mut a).await,
            Some(Outbound::Close { .. })
        ));
        assert!(matches!(
            next_item(&mut b).await,
            Some(Outbound::Close { .. })
        ));
        assert_eq!(bus.connection_count(), 0);
    }

    #[tokio::test]
    async fn dropping_a_connection_deregisters_it() {
        let bus = bus();
        let a = bus.register("a");
        let b = bus.register("a");
        assert_eq!(bus.connection_count(), 2);
        drop(a);
        assert_eq!(bus.connection_count(), 1);
        drop(b);
        assert_eq!(bus.connection_count(), 0);
        // The client's whole entry is gone, not left as an empty vector.
        bus.broadcast_to_client("a", &frame("x"));
    }

    #[tokio::test]
    async fn inbound_reaches_the_registered_handler() {
        struct Handler(Arc<Mutex<Vec<(String, String)>>>);
        impl InboundHandler for Handler {
            fn handle(&self, client_id: &str, kind: &str, _frame: serde_json::Value) {
                lock(&self.0).push((client_id.to_string(), kind.to_string()));
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let bus = bus();
        bus.on_inbound(Arc::new(Handler(Arc::clone(&seen))));

        let conn = bus.register("a");
        conn.deliver_inbound("chat.start", serde_json::json!({ "kind": "chat.start" }));
        assert_eq!(
            lock(&seen).as_slice(),
            [("a".to_string(), "chat.start".to_string())]
        );
    }

    #[tokio::test]
    async fn inbound_with_no_handler_is_dropped_rather_than_panicking() {
        let bus = bus();
        let conn = bus.register("a");
        conn.deliver_inbound("chat.start", serde_json::json!({}));
    }

    #[tokio::test]
    async fn a_seed_that_produces_nothing_is_fine() {
        struct Empty(Arc<AtomicUsize>);
        impl ConnectionSeed for Empty {
            fn seed(&self, _client_id: &str) -> Vec<ServerToClientFrame> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Vec::new()
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let bus = bus();
        bus.add_seed(Arc::new(Empty(Arc::clone(&calls))));
        let _conn = bus.register("a");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
