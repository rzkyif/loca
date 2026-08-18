//! The WebSocket half of the API: `POST /api/v1/ws/ticket` and `GET /ws/v1`.
//!
//! One socket per paired client, multiplexed. Every frame carries the
//! discriminator the client routes on (`streamId`, `callId`, `jobId`), so a
//! client needs exactly one connection however many streams are in flight.
//!
//! **Auth.** The upgrade presents a short-lived single-use ticket the client
//! minted over HTTP with its bearer. The long-lived bearer therefore never
//! travels in a `wss://` URL, where a reverse-proxy access log would capture
//! it; the worst a log can hold is a spent ticket.
//!
//! **Heartbeat.** The server pings every 25 seconds and expects a pong within
//! 10. These are application frames, not protocol pings, because the client's
//! frame handler is what answers them.
//!
//! **Frame policy: warn and drop, never close.** A malformed frame today
//! leaves the sending client hanging rather than telling it anything, so
//! tearing the socket down over one would turn a recoverable client bug into a
//! dead session. Every rejection below logs and continues, and the tests prove
//! the socket still works afterwards by sending a `ping` and expecting a
//! `pong`.

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, Instant};

use crate::contract::inbound::WsFrameEnvelope;

use super::frame_bus::{Connection, Outbound};
use super::http::Json;
use super::middleware::{from_fn_with_state, is_origin_allowed, require_client, AuthedClient};
use super::state::AppContext;

/// Server ping interval.
const HEARTBEAT: Duration = Duration::from_secs(25);
/// How long a client has to answer one.
const PONG_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one write to a socket may take before the connection is abandoned.
///
/// **This is what stops a client that stops reading from wedging its pump.**
/// Every write below happens inside the same `select!` as the heartbeat timer
/// and the pong deadline, so a `sink.send(..).await` that parks on a full TCP
/// window parks the whole loop with it: `conn.next()` stops being polled while
/// broadcasts keep queueing on an unbounded channel, and neither timer can fire
/// to end it, because both are arms of the future that is no longer being
/// polled. The pong timeout cannot rescue this, and reasoning that it would was
/// the specific mistake: it is downstream of the same blocked write.
///
/// Longer than [`PONG_TIMEOUT`] and than one [`HEARTBEAT`] interval, because a
/// write that has not completed in half a minute is not a slow client, it is a
/// client that is not reading at all. A live one drains a few-hundred-byte frame
/// far inside this.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// The schedule one socket's heartbeat runs on.
///
/// A value rather than the two constants read at their use sites, so a
/// socket-level test can drive the same loop on a scale it can watch in real
/// time. Every production path takes [`Heartbeat::default`], which is exactly
/// the two constants above; the only other way to build one is
/// `routes_with_heartbeat`, which exists in test builds alone.
#[derive(Clone, Copy)]
struct Heartbeat {
    interval: Duration,
    timeout: Duration,
    /// The per-write budget. On the same struct as the other two because a test
    /// that wants to observe a stalled writer has to compress it for the same
    /// reason it compresses the interval, and because all three bound the same
    /// loop.
    write_timeout: Duration,
}

impl Default for Heartbeat {
    fn default() -> Self {
        Self {
            interval: HEARTBEAT,
            timeout: PONG_TIMEOUT,
            write_timeout: WRITE_TIMEOUT,
        }
    }
}

/// Client frames are small control messages: a chat start, a tool response. A
/// payload wildly larger than any of them is refused before parsing, so a
/// hostile or buggy client cannot make the core parse an unbounded document.
///
/// Measured in bytes where the TypeScript measures UTF-16 code units. This is a
/// denial-of-service bound on an unparsed payload, not a wire contract, and one
/// megabyte is three orders of magnitude above any real frame.
const MAX_INBOUND_FRAME_BYTES: usize = 1_000_000;

/// Close codes. 4001 and 4002 are in the private range and the client
/// distinguishes them: a revoked client must not silently reconnect.
const CLOSE_REVOKED: u16 = 4001;
const CLOSE_PONG_TIMEOUT: u16 = 4002;

pub fn routes(ctx: &AppContext) -> Router {
    routes_with(ctx, Heartbeat::default())
}

/// The same two routes on a compressed heartbeat, so the end-to-end socket
/// tests can observe a ping and a pong deadline without waiting 25 real
/// seconds and without a virtual clock. The reason a virtual clock is not an
/// option is in the header of those tests, in `transport/tests.rs`.
#[cfg(test)]
pub(super) fn routes_with_heartbeat(
    ctx: &AppContext,
    interval: Duration,
    timeout: Duration,
) -> Router {
    routes_with(
        ctx,
        Heartbeat {
            interval,
            timeout,
            ..Heartbeat::default()
        },
    )
}

fn routes_with(ctx: &AppContext, heartbeat: Heartbeat) -> Router {
    let ticket = Router::new()
        .route("/api/v1/ws/ticket", post(mint_ticket))
        .layer(from_fn_with_state(ctx.clone(), require_client));
    Router::new()
        .merge(ticket)
        .route(
            "/ws/v1",
            get(
                move |state: State<AppContext>,
                      query: Query<UpgradeQuery>,
                      headers: HeaderMap,
                      ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>| {
                    upgrade(state, query, headers, ws, heartbeat)
                },
            ),
        )
        .with_state(ctx.clone())
}

#[derive(Serialize)]
struct TicketResponse {
    ticket: String,
}

/// Mint the single-use ticket for this client's next upgrade. The bearer rides
/// in this request's `Authorization` header, which a reverse proxy does not log
/// by default; only the throwaway ticket lands in a query string.
async fn mint_ticket(State(ctx): State<AppContext>, client: AuthedClient) -> Json<TicketResponse> {
    Json(TicketResponse {
        ticket: ctx.auth.mint_ws_ticket(&client.id),
    })
}

#[derive(Deserialize)]
struct UpgradeQuery {
    ticket: Option<String>,
}

async fn upgrade(
    State(ctx): State<AppContext>,
    Query(query): Query<UpgradeQuery>,
    headers: HeaderMap,
    // Deliberately a `Result`, so a failed upgrade extraction does not
    // short-circuit the checks below. Authentication runs BEFORE the handshake
    // is judged, which is what makes a plain `GET /ws/v1` with no ticket answer
    // 401 rather than "not a websocket request". The shipping behaviour, and
    // the more useful of the two answers.
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    heartbeat: Heartbeat,
) -> Response {
    // Mirror the HTTP origin allowlist on the upgrade. The ticket already
    // gates this, so rejecting a cross-origin browser upgrade is defence in
    // depth. Native clients send no Origin and pass.
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        if !is_origin_allowed(origin) {
            tracing::warn!(target: "loca::ws", "rejected ws upgrade from origin {origin}");
            return (StatusCode::FORBIDDEN, "forbidden origin").into_response();
        }
    }
    // These three rejections are plain text, not the error envelope. That is
    // what ships, the client does not parse them, and the upgrade path is not a
    // JSON API. The one deliberate exception to the envelope rule.
    let Some(ticket) = query.ticket else {
        return (StatusCode::UNAUTHORIZED, "missing ticket").into_response();
    };
    let Some(client_id) = ctx.auth.consume_ws_ticket(&ticket) else {
        return (StatusCode::UNAUTHORIZED, "invalid or expired ticket").into_response();
    };

    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };

    let conn = ctx.frames.register(&client_id);
    ws.on_upgrade(move |socket| serve(socket, conn, heartbeat))
}

/// Pump one socket until either side closes it.
///
/// Everything time-based lives in the one `select!` below rather than in
/// spawned timers, so there is no way for a timer to fire against a
/// half-closed socket.
async fn serve(socket: WebSocket, conn: Connection, heartbeat: Heartbeat) {
    let (sink, stream) = socket.split();
    pump(sink, stream, conn, heartbeat).await;
}

/// The loop itself, generic over the two halves of the socket.
///
/// Generic so a test can drive the REAL loop against a sink that never accepts
/// a write, which is the only way to observe that a stalled peer no longer
/// wedges the pump. Testing the write helper alone would prove the helper works
/// and say nothing about the loop it has to be used from, and the property here
/// is entirely about the loop: the timers and the writes share one future, so
/// what matters is that the future keeps making progress.
async fn pump<Si, St>(mut sink: Si, mut stream: St, mut conn: Connection, heartbeat: Heartbeat)
where
    Si: SinkExt<Message> + Unpin,
    St: StreamExt<Item = Result<Message, axum::Error>> + Unpin,
{
    let client_id = conn.client_id().to_string();

    let mut ping_at = Instant::now() + heartbeat.interval;
    // Armed when a ping goes out, disarmed by the pong. `None` means the client
    // owes us nothing.
    let mut pong_deadline: Option<Instant> = None;

    loop {
        // Copied out, not borrowed: the future below outlives the branch that
        // reassigns `pong_deadline` when a pong arrives.
        let deadline = pong_deadline;
        let tick = tokio::time::sleep_until(ping_at);
        let pong = async move {
            match deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                // Nothing pending: park forever and let the other branches win.
                None => std::future::pending().await,
            }
        };
        tokio::pin!(tick, pong);

        tokio::select! {
            outbound = conn.next() => match outbound {
                Some(Outbound::Frame(payload)) => {
                    let msg = Message::Text(Utf8Bytes::from(payload.as_ref()));
                    if !write(&mut sink, msg, heartbeat.write_timeout, &client_id).await {
                        break;
                    }
                }
                Some(Outbound::Close { code, reason }) => {
                    close(&mut sink, code, reason, heartbeat.write_timeout).await;
                    break;
                }
                // The bus dropped this connection without a code.
                None => break,
            },

            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match handle_client_frame(&conn, text.as_str()) {
                        Handled::Pong => pong_deadline = None,
                        Handled::Ping => {
                            let msg =
                                Message::Text(Utf8Bytes::from_static(r#"{"kind":"pong"}"#));
                            if !write(&mut sink, msg, heartbeat.write_timeout, &client_id).await {
                                break;
                            }
                        }
                        Handled::Nothing => {}
                    }
                }
                Some(Ok(Message::Binary(bytes))) => {
                    tracing::warn!(
                        target: "loca::ws",
                        "rejected ws frame: binary payload of {} bytes", bytes.len(),
                    );
                }
                Some(Ok(Message::Close(_))) | None => break,
                // Protocol-level ping and pong are answered by the library.
                Some(Ok(_)) => {}
                Some(Err(err)) => {
                    tracing::debug!(target: "loca::ws", "ws read failed for {client_id}: {err}");
                    break;
                }
            },

            _ = &mut tick => {
                let msg = Message::Text(Utf8Bytes::from_static(r#"{"kind":"ping"}"#));
                if !write(&mut sink, msg, heartbeat.write_timeout, &client_id).await {
                    break;
                }
                // Only arm a deadline when none is pending, so a second ping
                // does not extend the grace period of an unanswered first.
                pong_deadline.get_or_insert_with(|| Instant::now() + heartbeat.timeout);
                ping_at = Instant::now() + heartbeat.interval;
            }

            _ = &mut pong => {
                tracing::warn!(target: "loca::ws", "pong timeout for client {client_id}; closing");
                close(&mut sink, CLOSE_PONG_TIMEOUT, "pong timeout", heartbeat.write_timeout).await;
                break;
            }
        }
    }

    // Dropping `conn` deregisters it from the bus.
}

/// What [`handle_client_frame`] decided, for the transport to act on.
#[derive(Debug, PartialEq, Eq)]
pub enum Handled {
    /// A heartbeat reply: the client is alive.
    Pong,
    /// The client asked us for one.
    Ping,
    /// Dispatched, or dropped with a warning.
    Nothing,
}

/// The inbound frame policy: the size cap, the JSON parse, the envelope check,
/// the heartbeat split, and the hand-off to the registered handler.
///
/// **Public because it has two callers, and must never have two
/// implementations.** The socket pump below is one; the in-process pump in
/// `loca-core-ffi` is the other. This function is what decides whether a
/// `chat.start` frame reaches its handler at all, so a second copy would drift
/// with nothing going red, each half green against its own tests.
///
/// Every rejection returns [`Handled::Nothing`], which leaves the connection
/// open: a malformed frame today already leaves the sending client hanging, and
/// tearing the connection down over one turns a recoverable client bug into a
/// dead session.
pub fn handle_client_frame(conn: &Connection, text: &str) -> Handled {
    if text.len() > MAX_INBOUND_FRAME_BYTES {
        tracing::warn!(
            target: "loca::ws",
            "rejected ws frame: {} bytes exceeds {MAX_INBOUND_FRAME_BYTES}", text.len(),
        );
        return Handled::Nothing;
    }
    let raw: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(target: "loca::ws", "rejected ws frame: invalid JSON ({err})");
            return Handled::Nothing;
        }
    };
    // The envelope check, and the only parsing the transport does. Its
    // `Deserialize` carries the schema's constraints, so a `kind` that is
    // missing, not a string, or empty is rejected here. The per-kind types
    // belong to the handler that owns them, and deserializing one IS its
    // validation, so re-parsing here would either duplicate that or weaken it.
    let envelope: WsFrameEnvelope = match serde_json::from_value(raw.clone()) {
        Ok(envelope) => envelope,
        Err(err) => {
            tracing::warn!(target: "loca::ws", "rejected ws frame: envelope mismatch: {err}");
            return Handled::Nothing;
        }
    };

    match envelope.kind.as_str() {
        "ping" => Handled::Ping,
        "pong" => Handled::Pong,
        kind => {
            conn.deliver_inbound(kind, raw);
            Handled::Nothing
        }
    }
}

async fn close<S>(sink: &mut S, code: u16, reason: &'static str, budget: Duration)
where
    S: SinkExt<Message> + Unpin,
{
    // Budgeted for the same reason the frame writes are: this is reached from
    // the pong-timeout arm, so the peer it is trying to tell has already stopped
    // behaving. An unbounded close would hold the task open forever on exactly
    // the connection the timeout just decided to end. Dropping the sink after
    // this closes the socket regardless of whether the courtesy frame landed.
    let msg = Message::Close(Some(CloseFrame {
        code,
        reason: Utf8Bytes::from_static(reason),
    }));
    let _ = tokio::time::timeout(budget, sink.send(msg)).await;
}

/// One write, bounded by `budget`. `false` means the connection is finished:
/// either the peer went away or it stopped reading for longer than the budget.
///
/// Every write in `serve` goes through here, and that is the invariant rather
/// than a convenience: a single unbudgeted `sink.send(..).await` anywhere in
/// that `select!` reopens the wedge, because it is the same one future the
/// timers live in.
async fn write<S>(sink: &mut S, msg: Message, budget: Duration, client_id: &str) -> bool
where
    S: SinkExt<Message> + Unpin,
{
    match tokio::time::timeout(budget, sink.send(msg)).await {
        Ok(Ok(())) => true,
        // The ordinary end of a socket: the peer closed or the connection
        // broke. Not worth a warning.
        Ok(Err(_)) => false,
        Err(_) => {
            tracing::warn!(
                target: "loca::ws",
                "client {client_id} did not accept a write within {budget:?}; dropping the \
                 connection so its queued frames are released",
            );
            false
        }
    }
}

/// The close code a revoked client sees. Read by the pairing package when it
/// drops a client's connections, so the two cannot drift.
pub const REVOKED_CLOSE_CODE: u16 = CLOSE_REVOKED;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::transport::frame_bus::FrameBus;
    use std::sync::{Arc, Mutex};

    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl super::super::frame_bus::InboundHandler for Recorder {
        fn handle(&self, _client_id: &str, kind: &str, _frame: serde_json::Value) {
            self.0.lock().unwrap().push(kind.to_string());
        }
    }

    /// A peer that completed its upgrade and then stopped reading: every write
    /// parks forever, exactly as a real socket does once the TCP window fills.
    ///
    /// `poll_ready` is the pending one because that is where a full window
    /// actually stops a `SinkExt::send`.
    struct StalledSink;

    impl futures_util::Sink<Message> for StalledSink {
        type Error = axum::Error;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }

        fn start_send(self: std::pin::Pin<&mut Self>, _msg: Message) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_client_that_stops_reading_loses_its_connection_instead_of_wedging_the_pump() {
        // The failure this closes: the pump's delivery arm awaited
        // `sink.send(..)` with no deadline, so a peer that stopped draining
        // parked the entire `select!`. `conn.next()` was then never polled
        // again while broadcasts kept queueing on an UNBOUNDED channel, and
        // neither the heartbeat tick nor the pong deadline could end it,
        // because both are arms of the future that stopped being polled. The
        // connection survived indefinitely, growing.
        //
        // The observable is the pump RETURNING. A returned pump has dropped its
        // `Connection`, which deregisters it from the bus and releases the whole
        // queue; a wedged one never gets there. So the outer timeout below is
        // the assertion: without the per-write budget this test hangs until it
        // trips, which is the red.
        let bus = Arc::new(FrameBus::default());
        let conn = bus.register("client-a");

        // Enough queued frames that the pump has work it would happily block
        // on. One would do; a batch makes the intent obvious.
        for _ in 0..64 {
            bus.broadcast_to_client("client-a", &crate::contract::api::ServerToClientFrame::Pong);
        }

        let budget = Duration::from_millis(200);
        let heartbeat = Heartbeat {
            // Long enough that neither timer can be what ends the loop. If the
            // pump returns here it is because the WRITE gave up, which is the
            // property under test and not a heartbeat firing by luck.
            interval: Duration::from_secs(30),
            timeout: Duration::from_secs(30),
            write_timeout: budget,
        };

        let pumped = tokio::time::timeout(
            budget * 10,
            pump(
                StalledSink,
                futures_util::stream::pending(),
                conn,
                heartbeat,
            ),
        )
        .await;

        assert!(
            pumped.is_ok(),
            "the pump never returned for a client that stopped reading, so its frame queue grows \
             without bound and neither the heartbeat nor the pong timeout can close it: both are \
             arms of the same blocked future",
        );
    }

    fn conn_with_recorder() -> (Connection, Arc<Mutex<Vec<String>>>) {
        let bus = Arc::new(FrameBus::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        bus.on_inbound(Arc::new(Recorder(Arc::clone(&seen))));
        (bus.register("c"), seen)
    }

    fn dispatched(seen: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        seen.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn ping_is_answered_and_pong_clears_the_deadline() {
        let (conn, seen) = conn_with_recorder();
        assert!(matches!(
            handle_client_frame(&conn, r#"{"kind":"ping"}"#),
            Handled::Ping
        ));
        assert!(matches!(
            handle_client_frame(&conn, r#"{"kind":"pong"}"#),
            Handled::Pong
        ));
        // Neither is dispatched: the transport owns the heartbeat.
        assert!(dispatched(&seen).is_empty());
    }

    #[tokio::test]
    async fn a_known_frame_reaches_the_inbound_handler_unparsed() {
        let (conn, seen) = conn_with_recorder();
        handle_client_frame(
            &conn,
            r#"{"kind":"chat.start","streamId":"s","sessionId":"x"}"#,
        );
        assert_eq!(dispatched(&seen), ["chat.start"]);
    }

    /// An unknown kind is forwarded rather than dropped. The kind set lives
    /// with the handler that owns it, and a forward-compatible client may send
    /// something this build does not know about.
    #[tokio::test]
    async fn an_unknown_kind_is_forwarded_for_the_handler_to_judge() {
        let (conn, seen) = conn_with_recorder();
        handle_client_frame(&conn, r#"{"kind":"something.new"}"#);
        assert_eq!(dispatched(&seen), ["something.new"]);
    }

    #[tokio::test]
    async fn every_malformed_frame_is_dropped_and_nothing_is_dispatched() {
        let (conn, seen) = conn_with_recorder();
        for bad in [
            "not-json-at-all{",
            "",
            // No kind.
            r#"{"streamId":"s"}"#,
            // Kind of the wrong type.
            r#"{"kind":123}"#,
            // Empty kind: the schema's minLength is enforced by the generated
            // Deserialize, which is why there is no check for it here.
            r#"{"kind":""}"#,
            // Not an object.
            r#"[{"kind":"ping"}]"#,
            r#""ping""#,
        ] {
            assert!(
                matches!(handle_client_frame(&conn, bad), Handled::Nothing),
                "wrongly handled: {bad}"
            );
        }
        assert!(dispatched(&seen).is_empty());

        // The socket is still usable, which is the whole point of warn-and-drop.
        assert!(matches!(
            handle_client_frame(&conn, r#"{"kind":"ping"}"#),
            Handled::Ping
        ));
    }

    #[tokio::test]
    async fn an_oversized_frame_is_dropped_before_it_is_parsed() {
        let (conn, seen) = conn_with_recorder();
        let huge = format!(
            r#"{{"kind":"chat.start","pad":"{}"}}"#,
            "x".repeat(MAX_INBOUND_FRAME_BYTES)
        );
        assert!(matches!(
            handle_client_frame(&conn, &huge),
            Handled::Nothing
        ));
        assert!(dispatched(&seen).is_empty());
        // Exactly at the cap still parses, so the boundary is not off by one.
        let kind = r#"{"kind":"chat.start","pad":""#;
        let pad = MAX_INBOUND_FRAME_BYTES - kind.len() - 2;
        let at_cap = format!(r#"{{"kind":"chat.start","pad":"{}"}}"#, "x".repeat(pad));
        assert_eq!(at_cap.len(), MAX_INBOUND_FRAME_BYTES);
        handle_client_frame(&conn, &at_cap);
        assert_eq!(dispatched(&seen), ["chat.start"]);
    }
}
