//! The WebSocket half of the command boundary: `core_ws_{open,send,close}`.
//!
//! Shaped after the client's `net_ws_*` commands so the client-side local
//! transport is a near-copy of the one it already has, with three deliberate
//! differences that all come from there being no socket:
//!
//! - **No `url`.** The wire URL exists to carry `?ticket=`, and the ticket is
//!   precisely what the in-process identity replaces.
//! - **No `error` event.** Over a socket, `error` reports a connect failure.
//!   Registering with the frame bus cannot fail, which is the same fact that
//!   lets the client report connected immediately and skip its reconnect
//!   backoff entirely.
//! - **No heartbeat.** The socket pump pings every 25 seconds and closes on a
//!   10 second pong timeout because a TCP connection can die silently. Two
//!   objects in one process cannot, so nothing here pings and nothing times
//!   out. A client `ping` is still answered, so a shared client-side frame
//!   handler needs no special case.
//!
//! **The inbound frame policy is not reimplemented here.** Every payload goes
//! through `loca_core::transport::handle_client_frame`, the same function the
//! socket pump calls, so the size cap, the envelope check and the dispatch
//! cannot drift between the two.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use loca_core::transport::{handle_client_frame, Core, Handled, Outbound};

/// The `pong` reply to a client `ping`, byte for byte what the socket pump
/// sends. Part of the wire contract rather than a message of ours.
const PONG_FRAME: &str = r#"{"kind":"pong"}"#;

/// What the frontend delivers to the webview for one connection.
///
/// Mirrors the `net://ws/<id>/{open,message,close}` events, minus `error`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsEvent {
    /// Registered with the frame bus. Always the first event, and it cannot
    /// fail, which is why the client never needs to park or back off.
    Open,
    /// One rendered server frame.
    Message(String),
    /// The connection is gone. `code` is set only when the core closed it with
    /// one (a revoked client, or shutdown).
    Close { code: Option<u16> },
}

/// Where events go. Implemented over the Tauri `AppHandle` by the command
/// layer, and by a recorder in these tests.
///
/// A trait rather than a direct dependency on `tauri`, so the pump below, which
/// carries the ordering and lifetime rules, is exercised on the host.
pub trait WsSink: Send + Sync {
    fn emit(&self, ws_id: &str, event: WsEvent);
}

/// One item on a connection's inbound queue.
enum WsOut {
    Text(String),
    Close,
}

/// One registered connection: its outbound channel, and which registration it
/// is.
///
/// The generation makes the map safe to share between a pump that is shutting
/// down and one that just took its id: without it, a pump removing by key on the
/// way out can delete a live successor's entry.
struct Registered {
    generation: u64,
    tx: mpsc::UnboundedSender<WsOut>,
}

/// The live in-process connections, keyed by the id the webview generated.
/// Cheap to clone; every clone shares one map.
#[derive(Clone, Default)]
pub struct Connections {
    live: Arc<Mutex<HashMap<String, Registered>>>,
    next_generation: Arc<std::sync::atomic::AtomicU64>,
}

impl Connections {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `ws_id` with the frame bus and start pumping it.
    ///
    /// Registration and the `Open` event both happen HERE, synchronously,
    /// before the task is spawned, so a request the webview makes straight after
    /// this cannot race ahead of the registration, and no seed frame can arrive
    /// before `Open`: the bus queues seeds at register time, so emitting `Open`
    /// from inside the task would put it after them.
    ///
    /// # Re-opening a live id
    ///
    /// Re-using a live id REPLACES the old connection:
    ///
    /// - Inserting drops the old sender, which ends the old pump.
    /// - The old pump deregisters only if the entry is still ITS generation,
    ///   so it cannot delete the successor that replaced it.
    /// - The old pump emits no `Close`, because the id it would name now
    ///   belongs to a live connection.
    ///
    /// The naive version (insert blindly, remove by key) takes both connections
    /// down and leaves the webview with an `open` then a `close` on an id it
    /// believes is connected, which the client is told it need not guard
    /// against.
    pub fn open(&self, core: &Core, client_id: &str, ws_id: &str, sink: Arc<dyn WsSink>) {
        let conn = core.connect(client_id);
        let (tx, rx) = mpsc::unbounded_channel();
        let generation = self
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        lock(&self.live).insert(ws_id.to_string(), Registered { generation, tx });
        sink.emit(ws_id, WsEvent::Open);

        let registry = self.clone();
        let ws_id = ws_id.to_string();
        tokio::spawn(async move { pump(conn, rx, ws_id, generation, sink, registry).await });
    }

    /// Hand a client frame to its connection.
    ///
    /// An unknown id is ignored rather than an error: a send that races a close
    /// is ordinary on this boundary.
    pub fn send(&self, ws_id: &str, data: String) {
        if let Some(entry) = lock(&self.live).get(ws_id) {
            let _ = entry.tx.send(WsOut::Text(data));
        }
    }

    pub fn close(&self, ws_id: &str) {
        if let Some(entry) = lock(&self.live).get(ws_id) {
            let _ = entry.tx.send(WsOut::Close);
        }
    }

    /// Live connection count, for tests and diagnostics.
    pub fn len(&self) -> usize {
        lock(&self.live).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Deregister `ws_id`, but only if it is still `generation`'s.
    ///
    /// Returns whether this call owned the entry. `false` means a newer
    /// connection took the id over, so the caller must leave the map alone and
    /// emit no `Close` naming an id that is now live.
    fn remove_if_owned(&self, ws_id: &str, generation: u64) -> bool {
        let mut live = lock(&self.live);
        match live.get(ws_id) {
            Some(entry) if entry.generation == generation => {
                live.remove(ws_id);
                true
            }
            _ => false,
        }
    }
}

/// Pump one connection until either side ends it.
///
/// Dropping `conn` on the way out deregisters it from the bus, so an early
/// return cannot leak a registry entry.
///
/// **If this body ever grows a `?` or a fallible sink, move the deregistration
/// and the `Close` emit into a drop guard first.** Both run after the loop, so an
/// unwind would skip them and leave the "connected but dead" state the absence of
/// a heartbeat cannot detect. Unreachable today only because nothing here can
/// panic: `sink.emit` swallows its own failure, `handle_client_frame` is total,
/// and `lock` recovers from poisoning.
async fn pump(
    mut conn: loca_core::transport::Connection,
    mut rx: mpsc::UnboundedReceiver<WsOut>,
    ws_id: String,
    generation: u64,
    sink: Arc<dyn WsSink>,
    registry: Connections,
) {
    let mut close_code = None;
    loop {
        tokio::select! {
            outbound = conn.next() => match outbound {
                Some(Outbound::Frame(payload)) => {
                    sink.emit(&ws_id, WsEvent::Message(payload.to_string()));
                }
                Some(Outbound::Close { code, reason }) => {
                    tracing::debug!(
                        target: "loca::ws",
                        "in-process connection {ws_id} closed by the core: {code} {reason}",
                    );
                    close_code = Some(code);
                    break;
                }
                // The bus dropped this connection without a code.
                None => break,
            },

            inbound = rx.recv() => match inbound {
                Some(WsOut::Text(text)) => match handle_client_frame(&conn, &text) {
                    Handled::Ping => sink.emit(&ws_id, WsEvent::Message(PONG_FRAME.to_string())),
                    // We never ping, so a client pong answers nothing.
                    // Accepted silently, so a shared client-side heartbeat
                    // needs no local special case.
                    Handled::Pong => {}
                    Handled::Nothing => {}
                },
                Some(WsOut::Close) | None => break,
            },
        }
    }

    // Only if this pump still owns the id: after a newer `open`, both the
    // entry and the `Close` belong to that connection.
    if registry.remove_if_owned(&ws_id, generation) {
        sink.emit(&ws_id, WsEvent::Close { code: close_code });
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use loca_core::contract::api::ServerToClientFrame;
    use loca_core::transport::{testing, ConnectionSeed};

    const CLIENT: &str = "local";

    /// Records what the frontend would have sent to the webview.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<WsEvent>>);

    impl Recorder {
        fn events(&self) -> Vec<WsEvent> {
            lock(&self.0).clone()
        }

        /// Wait until `pred` holds, bounded: the pump is a spawned task, so an
        /// event that never arrives would hang the whole test binary rather
        /// than reporting red.
        async fn wait_for(&self, pred: impl Fn(&[WsEvent]) -> bool) -> Vec<WsEvent> {
            let deadline = std::time::Duration::from_secs(2);
            let poll = async {
                loop {
                    let events = self.events();
                    if pred(&events) {
                        return events;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            };
            match tokio::time::timeout(deadline, poll).await {
                Ok(events) => events,
                Err(_) => panic!("expected events never arrived; saw {:?}", self.events()),
            }
        }
    }

    impl WsSink for Recorder {
        fn emit(&self, _ws_id: &str, event: WsEvent) {
            lock(&self.0).push(event);
        }
    }

    struct OneSeed;

    impl ConnectionSeed for OneSeed {
        fn seed(&self, _client_id: &str) -> Vec<ServerToClientFrame> {
            vec![ServerToClientFrame::UpdateStaged {
                version: "seed".to_string(),
            }]
        }
    }

    fn messages(events: &[WsEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                WsEvent::Message(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn open_is_emitted_before_any_seed_frame() {
        // What the client's "report connected immediately" rests on. The bus
        // queues seeds at register time, so emitting `Open` from inside the
        // spawned task puts it after every seed, every run.
        let core = testing::core();
        core.frames().add_seed(Arc::new(OneSeed));
        let sink = Arc::new(Recorder::default());
        let conns = Connections::new();

        conns.open(&core, CLIENT, "w1", sink.clone());
        let events = sink
            .wait_for(|e| e.iter().any(|e| matches!(e, WsEvent::Message(_))))
            .await;

        assert_eq!(events[0], WsEvent::Open);
        assert!(messages(&events)[0].contains("seed"));
    }

    #[tokio::test]
    async fn a_broadcast_reaches_the_sink() {
        let core = testing::core();
        let sink = Arc::new(Recorder::default());
        let conns = Connections::new();
        conns.open(&core, CLIENT, "w1", sink.clone());

        core.frames().broadcast_to_client(
            CLIENT,
            &ServerToClientFrame::UpdateStaged {
                version: "9.9.9".to_string(),
            },
        );

        let events = sink
            .wait_for(|e| e.iter().any(|e| matches!(e, WsEvent::Message(_))))
            .await;
        assert!(messages(&events)[0].contains("9.9.9"));
    }

    #[tokio::test]
    async fn a_client_ping_is_answered_with_a_pong() {
        let core = testing::core();
        let sink = Arc::new(Recorder::default());
        let conns = Connections::new();
        conns.open(&core, CLIENT, "w1", sink.clone());

        conns.send("w1", r#"{"kind":"ping"}"#.to_string());

        let events = sink
            .wait_for(|e| e.iter().any(|e| matches!(e, WsEvent::Message(_))))
            .await;
        assert_eq!(messages(&events), vec![PONG_FRAME.to_string()]);
    }

    #[tokio::test]
    async fn a_client_pong_is_accepted_and_answers_nothing() {
        let core = testing::core();
        let sink = Arc::new(Recorder::default());
        let conns = Connections::new();
        conns.open(&core, CLIENT, "w1", sink.clone());

        conns.send("w1", r#"{"kind":"pong"}"#.to_string());
        // Then a ping, which produces one message: anything the pong produced
        // would be ahead of it.
        conns.send("w1", r#"{"kind":"ping"}"#.to_string());

        let events = sink
            .wait_for(|e| e.iter().any(|e| matches!(e, WsEvent::Message(_))))
            .await;
        assert_eq!(messages(&events), vec![PONG_FRAME.to_string()]);
    }

    #[tokio::test]
    async fn a_malformed_frame_is_dropped_and_the_connection_survives() {
        // The socket path's warn-and-drop policy, through the same function.
        let core = testing::core();
        let sink = Arc::new(Recorder::default());
        let conns = Connections::new();
        conns.open(&core, CLIENT, "w1", sink.clone());

        conns.send("w1", "not json at all".to_string());
        conns.send("w1", r#"{"kind":""}"#.to_string());
        conns.send("w1", r#"{"kind":"ping"}"#.to_string());

        let events = sink
            .wait_for(|e| e.iter().any(|e| matches!(e, WsEvent::Message(_))))
            .await;
        assert_eq!(messages(&events), vec![PONG_FRAME.to_string()]);
        assert!(!events.iter().any(|e| matches!(e, WsEvent::Close { .. })));
    }

    #[tokio::test]
    async fn close_deregisters_from_the_bus_and_the_registry() {
        let core = testing::core();
        let sink = Arc::new(Recorder::default());
        let conns = Connections::new();
        let before = core.frames().connection_count();

        conns.open(&core, CLIENT, "w1", sink.clone());
        sink.wait_for(|e| e.contains(&WsEvent::Open)).await;
        assert_eq!(core.frames().connection_count(), before + 1);
        assert_eq!(conns.len(), 1);

        conns.close("w1");
        sink.wait_for(|e| e.iter().any(|e| matches!(e, WsEvent::Close { .. })))
            .await;

        // Both sides, which fail independently: the bus entry leaks if the
        // `Connection` is not dropped, the registry entry if the pump forgets.
        wait_until(|| core.frames().connection_count() == before).await;
        wait_until(|| conns.is_empty()).await;
    }

    #[tokio::test]
    async fn a_core_side_close_carries_its_code() {
        let core = testing::core();
        let sink = Arc::new(Recorder::default());
        let conns = Connections::new();
        conns.open(&core, CLIENT, "w1", sink.clone());
        sink.wait_for(|e| e.contains(&WsEvent::Open)).await;

        core.frames().close_client(CLIENT, 4001, "revoked");

        let events = sink
            .wait_for(|e| e.iter().any(|e| matches!(e, WsEvent::Close { .. })))
            .await;
        assert_eq!(events.last(), Some(&WsEvent::Close { code: Some(4001) }));
    }

    #[tokio::test]
    async fn re_opening_a_live_id_replaces_it_and_leaves_the_new_one_working() {
        // The naive `open` (insert blindly, remove by key) takes BOTH
        // connections down: the old pump wakes and removes the successor's
        // entry, leaving the webview with `open` then `close` on an id it
        // believes is connected.
        let core = testing::core();
        let first = Arc::new(Recorder::default());
        let second = Arc::new(Recorder::default());
        let conns = Connections::new();

        conns.open(&core, CLIENT, "w1", first.clone());
        first.wait_for(|e| e.contains(&WsEvent::Open)).await;
        conns.open(&core, CLIENT, "w1", second.clone());
        second.wait_for(|e| e.contains(&WsEvent::Open)).await;

        // Exactly one live connection under that id, on both sides.
        wait_until(|| conns.len() == 1).await;
        wait_until(|| core.frames().connection_count() == 1).await;

        // The survivor is the NEW one, and it works.
        conns.send("w1", r#"{"kind":"ping"}"#.to_string());
        let events = second
            .wait_for(|e| e.iter().any(|e| matches!(e, WsEvent::Message(_))))
            .await;
        assert_eq!(messages(&events), vec![PONG_FRAME.to_string()]);

        // The superseded pump goes away silently: a `close` would name a live
        // id the webview cannot tell from its own connection dying.
        assert!(
            !first
                .events()
                .iter()
                .any(|e| matches!(e, WsEvent::Close { .. })),
            "the superseded connection announced a close for a live id: {:?}",
            first.events(),
        );
    }

    #[tokio::test]
    async fn closing_after_a_takeover_closes_only_the_live_connection() {
        // The other half of the generation guard: once the id has been taken
        // over, an explicit close must reach the survivor and report once.
        let core = testing::core();
        let first = Arc::new(Recorder::default());
        let second = Arc::new(Recorder::default());
        let conns = Connections::new();

        conns.open(&core, CLIENT, "w1", first.clone());
        first.wait_for(|e| e.contains(&WsEvent::Open)).await;
        conns.open(&core, CLIENT, "w1", second.clone());
        second.wait_for(|e| e.contains(&WsEvent::Open)).await;

        conns.close("w1");
        second
            .wait_for(|e| e.iter().any(|e| matches!(e, WsEvent::Close { .. })))
            .await;
        wait_until(|| conns.is_empty()).await;
        wait_until(|| core.frames().connection_count() == 0).await;
        assert!(!first
            .events()
            .iter()
            .any(|e| matches!(e, WsEvent::Close { .. })));
    }

    #[tokio::test]
    async fn a_domain_frame_reaches_the_inbound_handler() {
        // The one thing `handle_client_frame` exists to decide. Every other
        // inbound test here probes heartbeats and rejects, so a pump that
        // answered `ping` and dropped everything else would pass all of them.
        let core = testing::core();
        let inbound = Arc::new(RecordingInbound::default());
        core.frames().on_inbound(inbound.clone());
        let sink = Arc::new(Recorder::default());
        let conns = Connections::new();
        conns.open(&core, CLIENT, "w1", sink.clone());
        sink.wait_for(|e| e.contains(&WsEvent::Open)).await;

        conns.send("w1", r#"{"kind":"chat.start","streamId":"s1"}"#.to_string());

        inbound.wait_for_kind("chat.start").await;
        let (client_id, _, frame) = inbound.first();
        assert_eq!(client_id, CLIENT);
        // The WHOLE frame is handed over, not just the kind: the handler
        // deserializes its own per-kind type out of it.
        assert_eq!(frame["streamId"], "s1");
    }

    #[tokio::test]
    async fn a_frame_over_the_size_cap_is_dropped_and_the_connection_survives() {
        // The other half of the shared policy: the 1 MB denial-of-service
        // bound on an unparsed payload.
        let core = testing::core();
        let inbound = Arc::new(RecordingInbound::default());
        core.frames().on_inbound(inbound.clone());
        let sink = Arc::new(Recorder::default());
        let conns = Connections::new();
        conns.open(&core, CLIENT, "w1", sink.clone());
        sink.wait_for(|e| e.contains(&WsEvent::Open)).await;

        let huge = format!(
            r#"{{"kind":"chat.start","pad":"{}"}}"#,
            "x".repeat(1_000_001)
        );
        conns.send("w1", huge);
        // Then a well-formed one, so the assertion below is about the cap.
        conns.send(
            "w1",
            r#"{"kind":"chat.start","streamId":"small"}"#.to_string(),
        );

        inbound.wait_for_kind("chat.start").await;
        assert_eq!(inbound.count(), 1, "the oversized frame was delivered");
        assert_eq!(inbound.first().2["streamId"], "small");
        assert!(!sink
            .events()
            .iter()
            .any(|e| matches!(e, WsEvent::Close { .. })));
    }

    /// Records what the chat package would have received.
    #[derive(Default)]
    struct RecordingInbound(Mutex<Vec<(String, String, serde_json::Value)>>);

    impl RecordingInbound {
        fn count(&self) -> usize {
            lock(&self.0).len()
        }

        fn first(&self) -> (String, String, serde_json::Value) {
            lock(&self.0).first().cloned().expect("an inbound frame")
        }

        async fn wait_for_kind(&self, kind: &str) {
            let poll = async {
                loop {
                    if lock(&self.0).iter().any(|(_, k, _)| k == kind) {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            };
            if tokio::time::timeout(std::time::Duration::from_secs(2), poll)
                .await
                .is_err()
            {
                panic!("no `{kind}` frame reached the inbound handler");
            }
        }
    }

    impl loca_core::transport::InboundHandler for RecordingInbound {
        fn handle(&self, client_id: &str, kind: &str, frame: serde_json::Value) {
            lock(&self.0).push((client_id.to_string(), kind.to_string(), frame));
        }
    }

    #[tokio::test]
    async fn a_send_to_an_unknown_connection_is_ignored() {
        let conns = Connections::new();
        conns.send("nope", r#"{"kind":"ping"}"#.to_string());
        conns.close("nope");
        assert!(conns.is_empty());
    }

    #[tokio::test]
    async fn two_connections_are_independent() {
        let core = testing::core();
        let a = Arc::new(Recorder::default());
        let b = Arc::new(Recorder::default());
        let conns = Connections::new();
        conns.open(&core, CLIENT, "w1", a.clone());
        conns.open(&core, CLIENT, "w2", b.clone());
        a.wait_for(|e| e.contains(&WsEvent::Open)).await;
        b.wait_for(|e| e.contains(&WsEvent::Open)).await;

        conns.close("w1");
        a.wait_for(|e| e.iter().any(|e| matches!(e, WsEvent::Close { .. })))
            .await;
        wait_until(|| conns.len() == 1).await;

        // The survivor still works.
        conns.send("w2", r#"{"kind":"ping"}"#.to_string());
        let events = b
            .wait_for(|e| e.iter().any(|e| matches!(e, WsEvent::Message(_))))
            .await;
        assert_eq!(messages(&events), vec![PONG_FRAME.to_string()]);
    }

    async fn wait_until(pred: impl Fn() -> bool) {
        let poll = async {
            while !pred() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        };
        if tokio::time::timeout(std::time::Duration::from_secs(2), poll)
            .await
            .is_err()
        {
            panic!("condition never held");
        }
    }
}
