//! The chat suite, sectioned by the module under test.
//!
//! Two fixture rules throughout: a double never fills in what a test left out
//! (so a stub tool host can produce a controller that never resolves), and an
//! assertion that something did NOT happen is paired with one proving the
//! instrument would have noticed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use serde_json::{json, Map, Value};
use tempfile::TempDir;

use crate::contract::api::ServerToClientFrame;
use crate::paths::Paths;
use crate::sessions::doc::StoredMessage;
use crate::sessions::store::{CreateSessionInput, NewAttachment, SessionsStore};
use crate::settings::SettingsRecord;
use crate::transport::status::NoSidecars;
use crate::transport::{Connection, CoreStatusService, FrameBus, Outbound};

use super::deps::{ModelIdle, NoIdle, NoSchedule, NoTools, SchedulePersist, ToolHost};
use super::stream::{ActiveStreams, StreamGuard};
use super::writer::{LiveRef, TurnWriter};

const ALICE: &str = "client-alice";
const BOB: &str = "client-bob";

// --- fixtures ----------------------------------------------------------------

fn store() -> (TempDir, SessionsStore) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path());
    paths.ensure_dirs().unwrap();
    (tmp, SessionsStore::new(Arc::new(paths)))
}

async fn new_session(store: &SessionsStore, owner: &str) -> String {
    store
        .create(CreateSessionInput {
            owner_client_id: owner.to_string(),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
}

fn bus() -> Arc<FrameBus> {
    Arc::new(FrameBus::new())
}

/// Every frame queued on this connection right now, as parsed JSON.
///
/// Non-blocking on purpose: a test that drains with a timeout cannot tell "not
/// sent" from "sent late", and every send on this path is synchronous.
fn drain(conn: &mut Connection) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(Outbound::Frame(text)) = conn.next().now_or_never().flatten() {
        out.push(serde_json::from_str(&text).unwrap());
    }
    out
}

use futures_util::FutureExt as _;

fn kinds(frames: &[Value]) -> Vec<String> {
    frames
        .iter()
        .map(|f| f["kind"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// A `ModelIdle` that counts: `NoIdle` does nothing either way, so without this
/// "the claim notes activity" is unfalsifiable.
#[derive(Default)]
struct CountingIdle {
    activity: AtomicUsize,
    /// Every `on_turn_end` argument, in order. The VALUE matters: the guard must
    /// publish the count AFTER removing itself, so a single turn ending reports
    /// zero rather than one.
    turn_ends: Mutex<Vec<u32>>,
}

impl ModelIdle for CountingIdle {
    fn note_activity(&self) {
        self.activity.fetch_add(1, Ordering::Relaxed);
    }

    fn on_turn_end(&self, active_streams: u32) {
        self.turn_ends.lock().unwrap().push(active_streams);
    }

    fn ensure_loaded<'a>(
        &'a self,
        _settings: &'a SettingsRecord,
    ) -> BoxFuture<'a, crate::error::Result<()>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

fn streams() -> Arc<ActiveStreams> {
    Arc::new(ActiveStreams::new(Arc::new(NoIdle)))
}

fn streams_with(idle: Arc<CountingIdle>) -> Arc<ActiveStreams> {
    Arc::new(ActiveStreams::new(idle))
}

fn message(role: &str, id: &str) -> LiveRef {
    let Value::Object(map) = json!({
        "id": id,
        "ord": -1,
        "role": role,
        "content": "",
        "createdAtMs": 1_700_000_000_000i64,
    }) else {
        unreachable!()
    };
    Arc::new(Mutex::new(StoredMessage::new(map)))
}

fn prompt(call_id: &str) -> ServerToClientFrame {
    ServerToClientFrame::ToolAskuserRequest {
        call_id: call_id.to_string(),
        questions: Vec::new(),
        request_id: format!("req-{call_id}"),
    }
}

fn claim(
    streams: &Arc<ActiveStreams>,
    stream_id: &str,
    session: &str,
    client: &str,
) -> StreamGuard {
    streams.claim(stream_id, session, client).unwrap()
}

// --- stream.rs ---------------------------------------------------------------

#[tokio::test]
async fn a_second_claim_on_the_same_client_and_session_is_refused() {
    let streams = streams();
    let _first = claim(&streams, "s1", "sess-1", ALICE);

    assert!(streams.claim("s2", "sess-1", ALICE).is_none());
    // The instrument: the same call on a different session DOES succeed.
    assert!(streams.claim("s3", "sess-2", ALICE).is_some());
}

#[tokio::test]
async fn a_claim_by_a_different_client_on_the_same_session_is_allowed() {
    // Reads like a bug and is not: every frame is scoped to its owner, so two
    // paired clients can each run a turn on one session.
    let streams = streams();
    let _alice = claim(&streams, "s1", "sess-1", ALICE);

    assert!(streams.claim("s2", "sess-1", BOB).is_some());
}

#[tokio::test]
async fn three_live_messages_replay_in_birth_order() {
    // Three, not one: a `HashMap` reproduces a single-entry replay perfectly and
    // scrambles a three-entry one about half the time.
    let streams = streams();
    let guard = claim(&streams, "s1", "sess-1", ALICE);
    let stream = guard.stream();

    stream.note_born(
        "m-reasoning".into(),
        message("reasoning", "m-reasoning"),
        None,
    );
    stream.note_born(
        "m-assistant".into(),
        message("assistant", "m-assistant"),
        Some("m-reasoning".into()),
    );
    stream.note_born(
        "m-tool".into(),
        message("tool", "m-tool"),
        Some("m-assistant".into()),
    );

    let live = stream.live_messages();
    let ids: Vec<String> = live
        .iter()
        .map(|m| {
            m.message
                .lock()
                .unwrap()
                .id()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(ids, vec!["m-reasoning", "m-assistant", "m-tool"]);
    // The recorded `afterId` rides along, because it is the only way the client
    // can place a replayed message.
    let afters: Vec<Option<String>> = live.iter().map(|m| m.after_id.clone()).collect();
    assert_eq!(
        afters,
        vec![
            None,
            Some("m-reasoning".to_string()),
            Some("m-assistant".to_string())
        ]
    );
}

#[tokio::test]
async fn a_finalized_message_leaves_the_catch_up_buffer() {
    let streams = streams();
    let guard = claim(&streams, "s1", "sess-1", ALICE);
    let stream = guard.stream();

    stream.note_born("m1".into(), message("assistant", "m1"), None);
    stream.note_born("m2".into(), message("assistant", "m2"), Some("m1".into()));
    assert_eq!(stream.live_messages().len(), 2);

    stream.note_finalized("m1");

    let remaining: Vec<String> = stream
        .live_messages()
        .iter()
        .map(|m| {
            m.message
                .lock()
                .unwrap()
                .id()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(remaining, vec!["m2"]);
}

#[tokio::test]
async fn prompts_replay_in_the_order_they_were_opened() {
    let streams = streams();
    let guard = claim(&streams, "s1", "sess-1", ALICE);
    let stream = guard.stream();

    stream.retain_prompt("c1".into(), prompt("c1"));
    stream.retain_prompt("c2".into(), prompt("c2"));
    stream.retain_prompt("c3".into(), prompt("c3"));

    let ids: Vec<String> = stream
        .outstanding_prompts()
        .iter()
        .map(|f| match f {
            ServerToClientFrame::ToolAskuserRequest { call_id, .. } => call_id.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(ids, vec!["c1", "c2", "c3"]);
}

#[tokio::test]
async fn clear_prompt_drops_exactly_one() {
    let streams = streams();
    let guard = claim(&streams, "s1", "sess-1", ALICE);
    let stream = guard.stream();

    stream.retain_prompt("c1".into(), prompt("c1"));
    stream.retain_prompt("c2".into(), prompt("c2"));
    stream.clear_prompt("c1");

    assert_eq!(stream.outstanding_prompts().len(), 1);
}

#[tokio::test]
async fn the_guards_drop_deregisters_and_republishes_the_count() {
    let streams = streams();
    let status = CoreStatusService::new(Arc::new(NoSidecars));
    // Otherwise every snapshot is `starting_up` and the queue counts are
    // suppressed, so the assertion below would pass with the publish deleted.
    status.note_boot_done();
    streams.attach_status(status.clone());

    let guard = claim(&streams, "s1", "sess-1", ALICE);
    assert_eq!(streams.len(), 1);
    assert_eq!(
        status.snapshot().queues.map(|q| q.active_streams),
        Some(1),
        "claiming a stream publishes the count"
    );

    drop(guard);

    assert_eq!(streams.len(), 0);
    assert!(
        status.snapshot().queues.is_none(),
        "with no turn in flight the aggregate is idle again"
    );
}

#[tokio::test]
async fn the_guards_drop_fires_on_turn_end_with_the_post_removal_count() {
    let idle = Arc::new(CountingIdle::default());
    let streams = streams_with(Arc::clone(&idle));

    let first = claim(&streams, "s1", "sess-1", ALICE);
    let second = claim(&streams, "s2", "sess-2", ALICE);

    drop(second);
    drop(first);

    // One turn still running, then none. Publishing before the removal would
    // report 2 then 1, and the idle timer would never arm.
    assert_eq!(*idle.turn_ends.lock().unwrap(), vec![1, 0]);
}

#[tokio::test]
async fn a_claim_cancels_a_pending_idle_unload() {
    let idle = Arc::new(CountingIdle::default());
    let streams = streams_with(Arc::clone(&idle));

    let _guard = claim(&streams, "s1", "sess-1", ALICE);

    assert_eq!(idle.activity.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn active_session_ids_is_empty_once_every_guard_drops() {
    // The failure this pins has no symptom in the log: a leaked entry leaves the
    // background queue's idle gate shut forever, so memory summaries silently
    // stop running.
    let streams = streams();
    let first = claim(&streams, "s1", "sess-1", ALICE);
    let second = claim(&streams, "s2", "sess-2", BOB);

    assert_eq!(streams.active_session_ids().len(), 2);

    drop(first);
    drop(second);

    assert!(streams.active_session_ids().is_empty());
}

#[tokio::test]
async fn owned_refuses_a_stream_this_client_does_not_own() {
    let streams = streams();
    let _guard = claim(&streams, "s1", "sess-1", ALICE);

    assert!(streams.owned("s1", BOB).is_none());
    assert!(streams.owned("s1", ALICE).is_some());
    assert!(streams.owned_on_session(BOB, "sess-1").is_none());
    assert!(streams.owned_on_session(ALICE, "sess-1").is_some());
}

#[tokio::test]
async fn cancel_all_cancels_every_token() {
    let streams = streams();
    let first = claim(&streams, "s1", "sess-1", ALICE);
    let second = claim(&streams, "s2", "sess-2", ALICE);
    let third = claim(&streams, "s3", "sess-3", BOB);

    streams.cancel_all();

    assert!(first.stream().cancel.is_cancelled());
    assert!(second.stream().cancel.is_cancelled());
    assert!(third.stream().cancel.is_cancelled());
}

// --- writer.rs ---------------------------------------------------------------

/// A claimed turn with its writer, anchored on one user message, with the
/// connection already drained of the setup's own frames.
struct WriterEnv {
    _tmp: TempDir,
    _guard: StreamGuard,
    store: SessionsStore,
    session: String,
    anchor: String,
    writer: TurnWriter,
    conn: Connection,
}

async fn writer_env() -> WriterEnv {
    let (tmp, store) = store();
    let frames = bus();
    let streams = streams();
    let session = new_session(&store, ALICE).await;
    let anchor = store
        .insert_message_after(&session, user_message("hello"), None)
        .await
        .unwrap()
        .id;
    let guard = claim(&streams, "s1", &session, ALICE);
    let mut conn = frames.register(ALICE);
    let writer = TurnWriter::new(
        Arc::clone(guard.stream()),
        store.clone(),
        Arc::clone(&frames),
        Some(anchor.clone()),
    );
    let _ = drain(&mut conn);
    WriterEnv {
        _tmp: tmp,
        _guard: guard,
        store,
        session,
        anchor,
        writer,
        conn,
    }
}

impl WriterEnv {
    /// Every message id in the session store, in order.
    async fn ids(&self) -> Vec<String> {
        self.store
            .list_messages(&self.session)
            .await
            .iter()
            .map(|m| m.id().unwrap_or_default().to_string())
            .collect()
    }
}

fn user_message(text: &str) -> StoredMessage {
    let Value::Object(map) = json!({ "role": "user", "content": text }) else {
        unreachable!()
    };
    StoredMessage::new(map)
}

/// A store-shaped message, for seeding history the writer did not produce.
fn message_doc(role: &str, content: &str) -> StoredMessage {
    let Value::Object(map) = json!({ "role": role, "content": content }) else {
        unreachable!()
    };
    StoredMessage::new(map)
}

#[tokio::test]
async fn a_birth_carries_the_live_cursor_and_advances_it() {
    let mut env = writer_env().await;

    let reasoning = message("reasoning", "m-reasoning");
    env.writer.born(&reasoning);
    let assistant = message("assistant", "m-assistant");
    env.writer.born(&assistant);

    let sent = drain(&mut env.conn);
    assert_eq!(kinds(&sent), vec!["chat.message", "chat.message"]);
    assert_eq!(sent[0]["afterId"], json!(env.anchor));
    assert_eq!(sent[0]["final"], json!(false));
    // The second birth points at the first, not at the anchor: nothing is
    // persisted yet, so only the live cursor has moved.
    assert_eq!(sent[1]["afterId"], json!("m-reasoning"));
}

#[tokio::test]
async fn a_finalize_inserts_at_the_durable_cursor_not_the_live_one() {
    // Anchored MID-HISTORY (the regenerate case), the only fixture that can tell
    // the two cursors apart: on a tail-anchored turn both fall back to appending
    // and produce the identical transcript.
    let (_tmp, store) = store();
    let frames = bus();
    let streams = streams();
    let session = new_session(&store, ALICE).await;
    let anchor = store
        .insert_message_after(&session, user_message("regenerate this"), None)
        .await
        .unwrap()
        .id;
    let old_reply = store
        .insert_message_after(&session, message_doc("assistant", "old-reply"), None)
        .await
        .unwrap()
        .id;
    let later = store
        .insert_message_after(&session, user_message("and a later turn"), None)
        .await
        .unwrap()
        .id;

    let guard = claim(&streams, "s1", &session, ALICE);
    let mut writer = TurnWriter::new(
        Arc::clone(guard.stream()),
        store.clone(),
        Arc::clone(&frames),
        Some(anchor.clone()),
    );

    let reasoning = message("reasoning", "m-reasoning");
    writer.born(&reasoning);
    let assistant = message("assistant", "m-assistant");
    writer.born(&assistant);
    writer.finalize(&reasoning).await.unwrap();
    writer.finalize(&assistant).await.unwrap();

    let ids: Vec<String> = store
        .list_messages(&session)
        .await
        .iter()
        .map(|m| m.id().unwrap_or_default().to_string())
        .collect();
    // Both new messages land in the anchor's slot. Handing the store the live
    // cursor names a message it has never seen, which it resolves to "append",
    // and the turn lands after a conversation it is supposed to precede.
    assert_eq!(
        ids,
        vec![
            anchor,
            "m-reasoning".into(),
            "m-assistant".into(),
            old_reply,
            later
        ]
    );
}

#[tokio::test]
async fn a_born_then_finalized_message_carries_a_null_after_id() {
    let mut env = writer_env().await;

    let assistant = message("assistant", "m-assistant");
    env.writer.born(&assistant);
    env.writer.finalize(&assistant).await.unwrap();

    let sent = drain(&mut env.conn);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["final"], json!(true));
    // Already positioned by its birth. Re-sending a position would move a bubble
    // the client has already placed.
    assert_eq!(sent[1]["afterId"], Value::Null);
}

#[tokio::test]
async fn a_first_emission_finalize_carries_the_live_cursor_and_advances_it() {
    let mut env = writer_env().await;

    // The tool-call-only assistant: never born, so its finalize is what
    // positions it.
    let assistant = message("assistant", "m-assistant");
    env.writer.finalize(&assistant).await.unwrap();
    let second = message("assistant", "m-second");
    env.writer.finalize(&second).await.unwrap();

    let sent = drain(&mut env.conn);
    assert_eq!(sent[0]["afterId"], json!(env.anchor));
    assert_eq!(sent[0]["final"], json!(true));
    // Advanced, so the next unborn message lands after it rather than back at
    // the anchor.
    assert_eq!(sent[1]["afterId"], json!("m-assistant"));
}

#[tokio::test]
async fn the_two_cursors_reconverge_after_a_reasoning_assistant_pair() {
    let mut env = writer_env().await;

    let reasoning = message("reasoning", "m-reasoning");
    env.writer.born(&reasoning);
    let assistant = message("assistant", "m-assistant");
    env.writer.born(&assistant);
    env.writer.finalize(&reasoning).await.unwrap();
    env.writer.finalize(&assistant).await.unwrap();

    // Both cursors now name the assistant, so a message born in the next hop
    // lands after it.
    assert_eq!(env.writer.cursor(), Some("m-assistant"));
    let next = message("assistant", "m-next");
    env.writer.born(&next);
    env.writer.finalize(&next).await.unwrap();

    assert_eq!(
        env.ids().await[1..],
        ["m-reasoning", "m-assistant", "m-next"],
    );
}

#[tokio::test]
async fn the_assigned_ord_lands_on_the_live_object() {
    let mut env = writer_env().await;

    let assistant = message("assistant", "m-assistant");
    assert_eq!(assistant.lock().unwrap().ord(), Some(-1));
    env.writer.finalize(&assistant).await.unwrap();

    // The store assigns it, and the live object is what a resubscribe replays,
    // so the sentinel has to be overwritten in place.
    assert_eq!(assistant.lock().unwrap().ord(), Some(1));
}

#[tokio::test]
async fn a_message_that_fails_the_contract_is_logged_and_the_persist_survives() {
    let mut env = writer_env().await;

    // `role` is a closed enum on the wire, so this cannot be rendered as a
    // `chat.message`. Aborting the turn over it would lose every message after
    // it too.
    let broken = message("not-a-role", "m-broken");
    env.writer.finalize(&broken).await.unwrap();

    assert!(drain(&mut env.conn).is_empty(), "no frame could be sent");
    assert!(
        env.ids().await.contains(&"m-broken".to_string()),
        "but it was persisted"
    );
}

#[tokio::test]
async fn the_anchor_seeds_both_cursors() {
    let mut env = writer_env().await;

    assert_eq!(env.writer.cursor(), Some(env.anchor.as_str()));
    let assistant = message("assistant", "m-assistant");
    env.writer.born(&assistant);
    let sent = drain(&mut env.conn);
    // The live cursor was seeded too, so the first birth of the turn lands
    // immediately after the user message rather than at the top.
    assert_eq!(sent[0]["afterId"], json!(env.anchor));
}

// --- deps.rs -----------------------------------------------------------------

#[tokio::test]
async fn no_tools_phase2_returns_its_input_rather_than_an_empty_set() {
    // Failing open IS phase 2's contract: returning empty would make "no filter
    // ran" and "the filter rejected everything" indistinguishable.
    let host = NoTools;
    let candidates = vec![crate::contract::api::ToolDescriptor {
        description: "does a thing".into(),
        extension_id: "ext".into(),
        name: "tool".into(),
        similarity: None,
        tool_id: "ext::tool".into(),
    }];

    let out = host
        .filter_phase2(
            "query",
            candidates.clone(),
            &crate::llm::LlmEndpointConfig::default(),
            0,
        )
        .await;

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].tool_id, candidates[0].tool_id);
}

#[tokio::test]
async fn no_schedule_refuses_rather_than_reporting_success() {
    // The one outcome the persist-before-unblock ordering exists to prevent: a
    // tool told "scheduled" for a draft nothing stored.
    let draft = crate::contract::api::ScheduledPromptDraft {
        instruction: "summarise".into(),
        run_missed: false,
        schedule: crate::contract::api::ScheduleSpec::Interval { every_minutes: 60 },
        title: "nightly".into(),
    };

    assert!(NoSchedule.create(ALICE, &draft).is_err());
}

#[tokio::test]
async fn no_tools_catalog_reads_are_empty_rather_than_erroring() {
    // A core with no extension subsystem runs turns; it does not fail them.
    let host = NoTools;

    assert!(host.installed_extension_tools().await.is_empty());
    assert!(host.list_mcp_tools().await.is_empty());
    assert!(host.get_tool("ext::tool").await.is_none());
    assert!(host.load_tool_embeddings().await.is_empty());
    assert!(!host.is_mcp_server("ext").await);
    assert!(host.verify_tool_fresh("ext").await.is_ok());
    // The instrument: execution DOES refuse, so the emptiness above is a
    // deliberate catalog answer and not a trait that never does anything.
    assert!(host.call_mcp_tool("srv", "tool", Map::new()).await.is_err());
}

// --- muxer.rs ----------------------------------------------------------------

use crate::llm::{DeltaStream, LlmDelta, LlmRoute, ProviderError, ToolCallDelta};

use super::muxer::{ResolvedTool, StreamMuxer};

fn content(text: &str) -> LlmDelta {
    LlmDelta {
        content_delta: Some(text.to_string()),
        ..Default::default()
    }
}

fn reasoning(text: &str) -> LlmDelta {
    LlmDelta {
        reasoning_delta: Some(text.to_string()),
        ..Default::default()
    }
}

fn finish(reason: &str) -> LlmDelta {
    LlmDelta {
        finish_reason: Some(reason.to_string()),
        ..Default::default()
    }
}

fn call_chunk(index: u32, id: Option<&str>, name: Option<&str>, args: Option<&str>) -> LlmDelta {
    LlmDelta {
        tool_calls: vec![ToolCallDelta {
            index,
            id: id.map(str::to_string),
            name: name.map(str::to_string),
            arguments_delta: args.map(str::to_string),
        }],
        ..Default::default()
    }
}

fn scripted(items: Vec<LlmDelta>) -> DeltaStream {
    Box::pin(futures_util::stream::iter(
        items.into_iter().map(Ok).collect::<Vec<_>>(),
    ))
}

/// Every tool name resolves to one extension. A muxer test is never about the
/// catalog, and a resolver that refuses everything would turn each of these into
/// a test of the `unknown` path instead.
fn resolve_all(_name: &str) -> Option<ResolvedTool> {
    Some(ResolvedTool {
        extension_id: "ext".to_string(),
    })
}

fn resolve_none(_name: &str) -> Option<ResolvedTool> {
    None
}

struct MuxFixture {
    _tmp: TempDir,
    store: SessionsStore,
    frames: Arc<FrameBus>,
    _streams: Arc<ActiveStreams>,
    guard: StreamGuard,
    writer: TurnWriter,
    conn: Connection,
    muxer: StreamMuxer,
}

async fn mux_fixture() -> MuxFixture {
    let (tmp, store) = store();
    let frames = bus();
    let streams = streams();
    let session = new_session(&store, ALICE).await;
    let anchor = store
        .insert_message_after(&session, user_message("hello"), None)
        .await
        .unwrap()
        .id;
    let guard = claim(&streams, "s1", &session, ALICE);
    let mut conn = frames.register(ALICE);
    let _ = drain(&mut conn);
    let writer = TurnWriter::new(
        Arc::clone(guard.stream()),
        store.clone(),
        Arc::clone(&frames),
        Some(anchor),
    );
    let muxer = StreamMuxer::new(
        Arc::clone(guard.stream()),
        Arc::clone(&frames),
        LlmRoute::Default,
    );
    MuxFixture {
        _tmp: tmp,
        store,
        frames,
        _streams: streams,
        guard,
        writer,
        conn,
        muxer,
    }
}

/// `(kind, messageId, final)` for the message-shaped frames, which is the
/// ordering surface every muxer invariant is stated in.
fn trace(frames: &[Value]) -> Vec<String> {
    frames
        .iter()
        .map(|f| {
            let kind = f["kind"].as_str().unwrap_or_default();
            match kind {
                "chat.message" => format!(
                    "message {} {}",
                    f["message"]["role"].as_str().unwrap_or_default(),
                    if f["final"] == json!(true) {
                        "final"
                    } else {
                        "born"
                    }
                ),
                "chat.delta" => format!("delta {}", f["delta"].as_str().unwrap_or_default()),
                other => other.to_string(),
            }
        })
        .collect()
}

fn text_of(message: &Option<super::writer::LiveRef>) -> String {
    message
        .as_ref()
        .map(|m| {
            m.lock()
                .unwrap()
                .content()
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_default()
}

fn flag_of(message: &Option<super::writer::LiveRef>, key: &str) -> bool {
    message
        .as_ref()
        .map(|m| m.lock().unwrap().as_map().get(key) == Some(&json!(true)))
        .unwrap_or(false)
}

#[tokio::test]
async fn reasoning_finalizes_at_the_first_content_delta_not_after_the_stream() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![reasoning("thinking"), content("hello")]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;

    let sent = trace(&drain(&mut f.conn));
    // The thought bubble closes before a single token of the reply is on the
    // wire. Finalizing it on the way out instead leaves it open through the
    // whole reply, and no single-frame assertion notices.
    assert_eq!(
        sent,
        vec![
            "message reasoning born",
            "message assistant born",
            "delta thinking",
            "message reasoning final",
            "delta hello",
        ]
    );
    assert_eq!(text_of(&out.reasoning), "thinking");
}

#[tokio::test]
async fn a_reasoning_delta_after_the_first_content_delta_is_dropped() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![
                reasoning("first"),
                content("hello"),
                reasoning(" and more"),
            ]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;

    // The bubble already has its terminal snapshot. Appending would emit a
    // `chat.delta` for a closed id, which the client renders after the fact.
    assert_eq!(text_of(&out.reasoning), "first");
    let sent = trace(&drain(&mut f.conn));
    assert!(
        !sent.contains(&"delta  and more".to_string()),
        "no reasoning delta after the finalize: {sent:?}"
    );
    // The instrument: the reasoning that arrived in time DID reach the wire.
    assert!(sent.contains(&"delta first".to_string()), "{sent:?}");
}

#[tokio::test]
async fn a_chunk_carrying_both_content_and_reasoning_lands_its_content() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![LlmDelta {
                content_delta: Some("visible".into()),
                reasoning_delta: Some("hidden".into()),
                ..Default::default()
            }]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;

    // The reasoning arm is deliberately not an `else`: one chunk can carry both,
    // and making it one silently drops the content half.
    assert_eq!(text_of(&out.assistant), "visible");
    // This chunk's own reasoning loses, because content had already started by
    // the time the reasoning arm ran within the same chunk.
    assert!(out.reasoning.is_none());
}

#[tokio::test]
async fn the_live_object_advances_at_flush_never_at_append() {
    // The lag is what keeps a mid-stream `chat.subscribe` from replaying content
    // that the pending `chat.delta` is about to deliver again.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<LlmDelta, ProviderError>>();
    let f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();
    let stream = Arc::clone(f.guard.stream());
    let deltas: DeltaStream = Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }));

    let mut writer = f.writer;
    let muxer = f.muxer;
    let task =
        tokio::spawn(async move { muxer.run(deltas, &mut writer, &cancel, &resolve_all).await });

    tx.send(Ok(content("hello"))).unwrap();
    // Spin until the assistant is born, then read it in the same breath. The
    // coalesce window is 30ms of real time, so a snapshot taken this soon after
    // the birth is necessarily pre-flush.
    let live = loop {
        let live = stream.live_messages();
        if !live.is_empty() {
            break live;
        }
        tokio::task::yield_now().await;
    };
    let before = live[0]
        .message
        .lock()
        .unwrap()
        .content()
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    assert_eq!(before, "", "the live object lags the accumulator");

    drop(tx);
    let out = task.await.unwrap();
    // And it catches up: the accumulator was never the thing in doubt.
    assert_eq!(text_of(&out.assistant), "hello");
}

#[tokio::test]
async fn the_tail_flushes_on_the_error_path_too() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();
    let deltas: DeltaStream = Box::pin(futures_util::stream::iter(vec![
        Ok(content("half a sentence")),
        Err(ProviderError::transport("the socket went away")),
    ]));

    let out = f
        .muxer
        .run(deltas, &mut f.writer, &cancel, &resolve_all)
        .await;

    assert!(out.error.is_some());
    // A turn that fails mid-stream still owns the tokens it produced.
    assert_eq!(text_of(&out.assistant), "half a sentence");
    let sent = trace(&drain(&mut f.conn));
    assert!(
        sent.contains(&"delta half a sentence".to_string()),
        "{sent:?}"
    );
}

#[tokio::test]
async fn the_call_id_is_namespaced_by_stream_and_index() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![call_chunk(
                0,
                Some("call_alpha"),
                Some("do_thing"),
                Some("{\"a\":1}"),
            )]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;

    // Two concurrent turns whose models both emit `call_1` would otherwise
    // collide in the process-wide in-flight map, and a collision makes every
    // askUser and permission forward fail its owner check and hang both prompts.
    assert_eq!(out.tool_calls[0].call_id, "s1:0:call_alpha");
    assert_eq!(out.tool_calls[0].tool_name, "do_thing");
    assert_eq!(out.tool_calls[0].arguments, "{\"a\":1}");
    assert!(!out.tool_calls[0].unknown);
}

#[tokio::test]
async fn calls_resolve_in_index_order_whatever_order_their_chunks_arrive() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    // Three, and out of order: with two, an unstable map has an even chance of
    // looking right.
    let out = f
        .muxer
        .run(
            scripted(vec![
                call_chunk(2, Some("c"), Some("third"), None),
                call_chunk(0, Some("a"), Some("first"), None),
                call_chunk(1, Some("b"), Some("second"), None),
            ]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;

    let names: Vec<&str> = out
        .tool_calls
        .iter()
        .map(|c| c.tool_name.as_str())
        .collect();
    assert_eq!(names, vec!["first", "second", "third"]);
    let ids: Vec<&str> = out.tool_calls.iter().map(|c| c.call_id.as_str()).collect();
    assert_eq!(ids, vec!["s1:0:a", "s1:1:b", "s1:2:c"]);
}

#[tokio::test]
async fn arguments_accumulate_across_the_chunks_that_carry_them() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![
                call_chunk(0, Some("c1"), Some("t"), Some("{\"path\":")),
                call_chunk(0, None, None, Some("\"/tmp\"}")),
            ]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;

    assert_eq!(out.tool_calls.len(), 1);
    assert_eq!(out.tool_calls[0].arguments, "{\"path\":\"/tmp\"}");
}

#[tokio::test]
async fn absent_arguments_become_an_empty_object_not_an_empty_string() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![call_chunk(0, Some("c1"), Some("t"), None)]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;

    // `""` is not JSON. The dispatcher parses this before validating it against
    // the tool's schema, so an empty string fails the call rather than running a
    // no-argument tool.
    assert_eq!(out.tool_calls[0].arguments, "{}");
}

#[tokio::test]
async fn a_missing_model_call_id_is_minted_rather_than_left_empty() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![call_chunk(0, None, Some("t"), None)]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;

    let id = &out.tool_calls[0].call_id;
    assert!(id.starts_with("s1:0:"), "{id}");
    // Defaulting instead would give every id-less call of a stream the same
    // namespace, which is the collision the namespace exists to prevent.
    assert_eq!(id.len(), "s1:0:".len() + 26);
}

#[tokio::test]
async fn an_unresolvable_tool_name_is_dispatched_as_unknown_rather_than_dropped() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![call_chunk(0, Some("c1"), Some("nope"), None)]),
            &mut f.writer,
            &cancel,
            &resolve_none,
        )
        .await;

    // Dropping it would leave the model waiting for a result that never comes.
    assert_eq!(out.tool_calls.len(), 1);
    assert!(out.tool_calls[0].unknown);
    assert_eq!(out.tool_calls[0].extension_id, "unknown");
}

#[tokio::test]
async fn a_tool_call_only_hop_still_synthesises_an_assistant() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![call_chunk(0, Some("c1"), Some("t"), None)]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;

    // The persisted `toolCalls` are what lets a transcript rebuild replay them.
    // Without an assistant message every following `tool` message is orphaned.
    let assistant = out
        .assistant
        .expect("a tool-call-only hop has an assistant");
    let calls = assistant.lock().unwrap().as_map()["toolCalls"].clone();
    assert_eq!(calls[0]["callId"], json!("s1:0:c1"));
    assert_eq!(calls[0]["status"], json!("pending"));
}

#[tokio::test]
async fn truncation_needs_a_length_finish_and_no_tool_calls() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![content("cut off here"), finish("length")]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;
    assert!(out.truncated);
    assert!(flag_of(&out.assistant, "truncated"));

    // A tool hop finishes `tool_calls`, not `length`, and a provider that says
    // `length` anyway has not truncated the reply: the reply is the calls.
    let mut g = mux_fixture().await;
    let cancel = g.guard.stream().cancel.clone();
    let out = g
        .muxer
        .run(
            scripted(vec![
                call_chunk(0, Some("c1"), Some("t"), None),
                finish("length"),
            ]),
            &mut g.writer,
            &cancel,
            &resolve_all,
        )
        .await;
    assert!(!out.truncated);
}

#[tokio::test]
async fn a_truncated_hop_with_no_content_still_gets_a_message_to_carry_the_flag() {
    // Thinking consumed the whole window, so there is no reply at all. The
    // client renders "cut off" from the flag, and with no message there is
    // nothing to render it on.
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![reasoning("thinking..."), finish("length")]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;

    assert!(out.truncated);
    assert!(flag_of(&out.assistant, "truncated"));
    assert_eq!(text_of(&out.assistant), "");
}

#[tokio::test]
async fn empty_content_deltas_do_not_born_an_assistant() {
    // The opening chunk of several providers carries a role and an empty string.
    // Borning on it puts an empty bubble in every transcript.
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let out = f
        .muxer
        .run(
            scripted(vec![content(""), reasoning("")]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;

    assert!(out.assistant.is_none());
    assert!(out.reasoning.is_none());
    assert!(drain(&mut f.conn).is_empty());
}

#[tokio::test]
async fn usage_is_broadcast_once_and_only_when_the_provider_reported_it() {
    let mut f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();

    let _ = f
        .muxer
        .run(
            scripted(vec![content("hi"), finish("stop")]),
            &mut f.writer,
            &cancel,
            &resolve_all,
        )
        .await;
    let sent = kinds(&drain(&mut f.conn));
    assert!(!sent.contains(&"chat.usage".to_string()), "{sent:?}");

    // The instrument: with usage reported, exactly one frame goes out.
    let mut g = mux_fixture().await;
    let cancel = g.guard.stream().cancel.clone();
    let _ = g
        .muxer
        .run(
            scripted(vec![
                content("hi"),
                LlmDelta {
                    usage: Some(crate::contract::api::TokenUsage {
                        completion: 2,
                        prompt: 7,
                        total: 9,
                    }),
                    ..Default::default()
                },
            ]),
            &mut g.writer,
            &cancel,
            &resolve_all,
        )
        .await;
    let sent = kinds(&drain(&mut g.conn));
    assert_eq!(
        sent.iter().filter(|k| *k == "chat.usage").count(),
        1,
        "{sent:?}"
    );
}

#[tokio::test]
async fn an_interrupted_partial_assistant_is_flagged_persisted_and_returned() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<LlmDelta, ProviderError>>();
    let f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();
    let stream = Arc::clone(f.guard.stream());
    let deltas: DeltaStream = Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }));

    let mut writer = f.writer;
    let muxer = f.muxer;
    let task = tokio::spawn(async move {
        let out = muxer.run(deltas, &mut writer, &cancel, &resolve_all).await;
        (out, writer)
    });

    tx.send(Ok(reasoning("half a thought"))).unwrap();
    tx.send(Ok(content("half a sentence"))).unwrap();
    while stream.live_messages().len() < 2 {
        tokio::task::yield_now().await;
    }
    stream.cancel.cancel();
    // The channel stays open: an interrupt is not an end-of-stream, and a test
    // that closed it here would be testing the ordinary exit instead.
    let (out, mut writer) = task.await.unwrap();

    assert!(out.interrupted);
    assert_eq!(text_of(&out.assistant), "half a sentence");
    assert!(flag_of(&out.assistant, "interrupted"));
    // The thought bubble is NOT flagged: content had already started, so it was
    // closed normally at the first content delta and the later interrupt cannot
    // reopen it. The neighbouring test covers an interrupt that lands while
    // reasoning is still the only thing streaming.
    assert!(!flag_of(&out.reasoning, "interrupted"));
    writer
        .finalize(out.assistant.as_ref().unwrap())
        .await
        .unwrap();
    let roles: Vec<String> = f
        .store
        .list_messages(&f.guard.stream().session_id)
        .await
        .iter()
        .map(|m| m.role().unwrap_or_default().to_string())
        .collect();
    assert_eq!(roles, vec!["user", "reasoning", "assistant"]);
    let _ = f.frames;
}

#[tokio::test]
async fn an_interrupt_during_reasoning_closes_the_thought_bubble_as_interrupted() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<LlmDelta, ProviderError>>();
    let f = mux_fixture().await;
    let cancel = f.guard.stream().cancel.clone();
    let stream = Arc::clone(f.guard.stream());
    let deltas: DeltaStream = Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }));

    let mut writer = f.writer;
    let muxer = f.muxer;
    let task =
        tokio::spawn(async move { muxer.run(deltas, &mut writer, &cancel, &resolve_all).await });

    tx.send(Ok(reasoning("half a thought"))).unwrap();
    while stream.live_messages().is_empty() {
        tokio::task::yield_now().await;
    }
    stream.cancel.cancel();
    let out = task.await.unwrap();

    assert!(out.interrupted);
    assert!(out.assistant.is_none(), "nothing was ever said");
    assert_eq!(text_of(&out.reasoning), "half a thought");
    assert!(flag_of(&out.reasoning, "interrupted"));
    // Persisted on the way out, so a reload shows the same partial thought the
    // client watched arrive.
    let roles: Vec<String> = f
        .store
        .list_messages(&f.guard.stream().session_id)
        .await
        .iter()
        .map(|m| m.role().unwrap_or_default().to_string())
        .collect();
    assert_eq!(roles, vec!["user", "reasoning"]);
}

// --- transcript.rs -----------------------------------------------------------

use crate::contract::api::MemoryKind;
use crate::db::{migrate, Db};
use crate::llm::{ChatMessage, ContentPart, UserContent};
use crate::memories::MemoriesStore;
use crate::sessions::attachments::AttachmentCache;

use super::transcript::{to_chat_messages, TranscriptSources};

struct TranscriptEnv {
    _tmp: TempDir,
    sessions: SessionsStore,
    memories: MemoriesStore,
    session_id: String,
}

async fn transcript_env() -> TranscriptEnv {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Arc::new(Paths::rooted(tmp.path()));
    paths.ensure_dirs().unwrap();
    let db = Db::open_in_memory().unwrap();
    db.call_blocking(|conn| migrate(conn)).unwrap();
    let sessions = SessionsStore::new(Arc::clone(&paths));
    let memories = MemoriesStore::new(paths, db);
    let session_id = new_session(&sessions, ALICE).await;
    TranscriptEnv {
        _tmp: tmp,
        sessions,
        memories,
        session_id,
    }
}

impl TranscriptEnv {
    fn sources(&self) -> TranscriptSources<'_> {
        TranscriptSources {
            sessions: &self.sessions,
            memories: &self.memories,
            session_id: &self.session_id,
        }
    }

    async fn build(&self, history: &[StoredMessage], system: &str) -> Vec<ChatMessage> {
        let mut cache = AttachmentCache::new();
        to_chat_messages(
            &self.sources(),
            history,
            system,
            &std::collections::HashSet::new(),
            &mut cache,
        )
        .await
        .unwrap()
    }
}

fn stored(value: Value) -> StoredMessage {
    let Value::Object(map) = value else {
        unreachable!()
    };
    StoredMessage::new(map)
}

fn roles(messages: &[ChatMessage]) -> Vec<&'static str> {
    messages
        .iter()
        .map(|m| match m {
            ChatMessage::System { .. } => "system",
            ChatMessage::User { .. } => "user",
            ChatMessage::Assistant { .. } => "assistant",
            ChatMessage::Tool { .. } => "tool",
        })
        .collect()
}

fn user_text(message: &ChatMessage) -> String {
    match message {
        ChatMessage::User {
            content: UserContent::Text(text),
        } => text.clone(),
        other => panic!("expected plain user text, got {other:?}"),
    }
}

fn user_parts(message: &ChatMessage) -> Vec<ContentPart> {
    match message {
        ChatMessage::User {
            content: UserContent::Parts(parts),
        } => parts.clone(),
        other => panic!("expected multipart user content, got {other:?}"),
    }
}

#[tokio::test]
async fn the_system_prompt_leads_and_an_empty_one_is_omitted() {
    let env = transcript_env().await;
    let history = vec![stored(json!({ "role": "user", "content": "hi" }))];

    let with = env.build(&history, "you are a helpful assistant").await;
    assert_eq!(roles(&with), vec!["system", "user"]);

    // An empty prompt is absent, not an empty system message: several providers
    // treat a blank system turn as an instruction to say nothing.
    let without = env.build(&history, "").await;
    assert_eq!(roles(&without), vec!["user"]);
}

#[tokio::test]
async fn the_bubbles_the_model_never_sees_are_dropped() {
    let env = transcript_env().await;
    let history = vec![
        stored(json!({ "role": "user", "content": "hi" })),
        stored(json!({ "role": "reasoning", "content": "thinking" })),
        stored(json!({ "role": "tool_filter", "status": "ok" })),
        stored(json!({ "role": "memory_filter", "status": "ok" })),
        stored(json!({ "role": "display", "content": { "kind": "text", "text": "x" } })),
        stored(json!({ "role": "error", "content": "boom" })),
        stored(json!({ "role": "assistant", "content": "hello" })),
    ];

    // Reasoning is not part of the message protocol, and the four client-facing
    // bubbles would read as instructions if they reached the model.
    assert_eq!(
        roles(&env.build(&history, "").await),
        vec!["user", "assistant"]
    );
}

#[tokio::test]
async fn an_assistant_replays_the_tool_calls_it_emitted() {
    let env = transcript_env().await;
    let history = vec![
        stored(json!({ "role": "user", "content": "run it" })),
        stored(json!({
            "role": "assistant",
            "content": "",
            "toolCalls": [
                { "callId": "s1:0:c1", "extensionId": "ext", "toolName": "do_thing",
                  "arguments": "{\"a\":1}", "status": "completed" }
            ]
        })),
        stored(json!({
            "role": "tool", "callId": "s1:0:c1", "extensionId": "ext",
            "toolName": "do_thing", "arguments": "{}", "status": "completed",
            "result": { "ok": true }
        })),
    ];

    let out = env.build(&history, "").await;
    // Not replaying them orphans the `tool` message that follows, and an
    // orphaned `tool_call_id` garbles every hop after the call.
    let ChatMessage::Assistant { tool_calls, .. } = &out[1] else {
        panic!("expected an assistant")
    };
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "s1:0:c1");
    assert_eq!(tool_calls[0].function.name, "do_thing");
    assert_eq!(tool_calls[0].function.arguments, "{\"a\":1}");
    // And the pair matches: the tool result names the same id.
    let ChatMessage::Tool { tool_call_id, .. } = &out[2] else {
        panic!("expected a tool message")
    };
    assert_eq!(tool_call_id, "s1:0:c1");
}

#[tokio::test]
async fn a_tool_result_carries_the_raw_value_and_a_failure_carries_only_the_error() {
    let env = transcript_env().await;
    let history = vec![
        stored(json!({
            "role": "tool", "callId": "c-ok", "extensionId": "ext", "toolName": "t",
            "arguments": "{}", "status": "completed", "result": { "rows": 3 }
        })),
        stored(json!({
            "role": "tool", "callId": "c-bad", "extensionId": "ext", "toolName": "t",
            "arguments": "{}", "status": "failed", "error": "not available"
        })),
        stored(json!({
            "role": "tool", "callId": "c-silent", "extensionId": "ext", "toolName": "t",
            "arguments": "{}", "status": "failed"
        })),
    ];

    let out = env.build(&history, "").await;
    let body = |m: &ChatMessage| match m {
        ChatMessage::Tool { content, .. } => content.clone(),
        other => panic!("expected a tool message, got {other:?}"),
    };
    assert_eq!(body(&out[0]), "{\"rows\":3}");
    assert_eq!(body(&out[1]), "{\"error\":\"not available\"}");
    // An absent error serialises as `{}`, not `{"error":null}`: the second tells
    // the model the tool reported a null error, which is a different claim.
    assert_eq!(body(&out[2]), "{}");
}

#[tokio::test]
async fn a_multipart_message_of_only_text_collapses_to_a_string() {
    let env = transcript_env().await;
    let history = vec![stored(json!({
        "role": "user",
        "content": [
            { "type": "text", "text": "look at this" },
            { "type": "document", "filename": "notes.md", "markdown": "# Notes" }
        ]
    }))];

    let out = env.build(&history, "").await;
    // Some providers reject a parts array with no `image_url` sibling, so a
    // text-only message must not use the multipart form at all.
    assert_eq!(
        user_text(&out[0]),
        "look at this\n[Attached document: notes.md]\n\n# Notes"
    );
}

#[tokio::test]
async fn an_image_attachment_is_inlined_as_a_data_uri_and_keeps_the_parts_form() {
    let env = transcript_env().await;
    let abs_path = env._tmp.path().join("shot.png");
    tokio::fs::write(&abs_path, [1u8, 2, 3]).await.unwrap();
    let attachment = env
        .sessions
        .record_attachment(
            &env.session_id,
            NewAttachment {
                message_id: "m-user".into(),
                filename: "shot.png".into(),
                mime: Some("image/png".into()),
                size_bytes: 3,
                abs_path: abs_path.to_string_lossy().into_owned(),
            },
        )
        .await
        .unwrap();
    let path = format!(
        "https://core.local/api/v1/sessions/{}/attachments/{}",
        env.session_id, attachment.id
    );
    let history = vec![stored(json!({
        "role": "user",
        "content": [
            { "type": "text", "text": "what is this" },
            { "type": "image_file", "filename": "shot.png", "mime": "image/png", "path": path }
        ]
    }))];

    let parts = user_parts(&env.build(&history, "").await[0]);
    assert_eq!(parts.len(), 2);
    match &parts[1] {
        ContentPart::ImageUrl { image_url } => {
            assert_eq!(image_url.url, "data:image/png;base64,AQID");
        }
        other => panic!("expected an image part, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unreadable_attachment_is_skipped_rather_than_failing_the_turn() {
    let env = transcript_env().await;
    let history = vec![stored(json!({
        "role": "user",
        "content": [
            { "type": "text", "text": "and this one" },
            { "type": "image_file", "filename": "gone.png", "mime": "image/png",
              "path": "https://core.local/api/v1/sessions/s/attachments/does-not-exist" }
        ]
    }))];

    // One missing file must not lose the question it was attached to.
    let out = env.build(&history, "").await;
    assert_eq!(user_text(&out[0]), "and this one");
}

#[tokio::test]
async fn a_token_memory_reference_is_expanded_into_the_user_message() {
    let env = transcript_env().await;
    env.memories
        .create(MemoryKind::Knowledge, "notes", "the sky is green")
        .await
        .unwrap();
    let history = vec![stored(json!({ "role": "user", "content": "check @notes" }))];

    let text = user_text(&env.build(&history, "").await[0]);
    // The stored message keeps only the token; the CURRENT content is appended
    // at request-build time, so editing the memory flows into the next turn.
    assert!(text.starts_with("check @notes"), "{text}");
    assert!(text.contains("the sky is green"), "{text}");
}

#[tokio::test]
async fn a_slug_an_mcp_pass_already_claimed_is_not_expanded_twice() {
    let env = transcript_env().await;
    env.memories
        .create(MemoryKind::Knowledge, "notes", "the sky is green")
        .await
        .unwrap();
    let history = vec![stored(json!({ "role": "user", "content": "check @notes" }))];
    let mut claimed = std::collections::HashSet::new();
    claimed.insert("notes".to_string());
    let mut cache = AttachmentCache::new();

    let out = to_chat_messages(&env.sources(), &history, "", &claimed, &mut cache)
        .await
        .unwrap();

    // MCP wins: the resource block is already in the request, and expanding the
    // memory too would inject the same slug twice.
    assert_eq!(user_text(&out[0]), "check @notes");
}

// --- tools/dispatch.rs -------------------------------------------------------

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::contract::api::{
    AskUserAnswer, DisplayContent, PermissionKind, ScheduledPromptDraft, Tool, ToolProviderKind,
};
use crate::error::{AppError, ErrorCode};

use super::deps::{ToolCallController, ToolCallEvent, ToolCallSpec};
use super::muxer::PendingCall;
use super::tools::{Dispatcher, InFlight};

fn stub_tool(extension_id: &str, name: &str) -> Tool {
    Tool {
        always_available: false,
        description: "does a thing".into(),
        enabled: true,
        extension_id: extension_id.into(),
        fn_export: "default".into(),
        grants: Vec::new(),
        id: format!("{extension_id}::{name}"),
        missing_required: Vec::new(),
        name: name.into(),
        parameters: Map::new(),
        platforms: Vec::new(),
        provider_kind: ToolProviderKind::Extension,
        provider_name: None,
        required_permissions: Vec::new(),
        triggers: Vec::new(),
    }
}

/// What the stub host is scripted with. Every field is explicit: a double that
/// fills in what a test left out hides the branch that depends on its absence.
#[derive(Default)]
struct Script {
    tools: HashMap<String, Tool>,
    /// What `list_mcp_tools` returns. Separate from `tools` because the MCP
    /// append reads only this and the extension pipeline reads only that; one
    /// map for both would make a name collision untestable.
    mcp_tools: Vec<Tool>,
    mcp_servers: HashSet<String>,
    mcp_result: Option<Result<Value, String>>,
    verify_error: Option<String>,
    validate_error: Option<String>,
    start_error: Option<String>,
    events: Vec<ToolCallEvent>,
    /// `None` means the controller never resolves on its own, which is the only
    /// way the cancellation path is reachable.
    outcome: Option<Result<Value, String>>,
    /// Whether a cancel makes the never-resolving controller finish. `false`
    /// models a worker that ignores its cancel.
    cancel_resolves: bool,
}

struct StubToolHost {
    script: Mutex<Script>,
    cancels: Arc<AtomicUsize>,
    starts: Arc<AtomicUsize>,
    /// Set once a call starts, so a test can wait for the worker to be running
    /// without a sleep.
    started: Arc<tokio::sync::Notify>,
}

impl StubToolHost {
    fn new(script: Script) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script),
            cancels: Arc::new(AtomicUsize::new(0)),
            starts: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(tokio::sync::Notify::new()),
        })
    }
}

struct StubController {
    outcome: Mutex<Option<Result<Value, String>>>,
    resolved: Arc<tokio::sync::Notify>,
    cancels: Arc<AtomicUsize>,
    cancel_resolves: bool,
    schedule_requests: Mutex<HashSet<String>>,
    answers: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ToolCallController for StubController {
    async fn done(&self) -> crate::error::Result<Value> {
        let scripted = self.outcome.lock().unwrap().take();
        if let Some(outcome) = scripted {
            return outcome.map_err(|e| AppError::new(ErrorCode::InternalError, e));
        }
        // Never resolves on its own. A cancel is the only thing that ends it,
        // and a script that says otherwise models a worker ignoring its cancel.
        self.resolved.notified().await;
        Err(AppError::new(ErrorCode::InternalError, "cancelled"))
    }

    fn cancel(&self) {
        self.cancels.fetch_add(1, Ordering::Relaxed);
        if self.cancel_resolves {
            self.resolved.notify_one();
        }
    }

    fn respond_ask_user(&self, request_id: &str, _answers: Vec<AskUserAnswer>) {
        self.answers
            .lock()
            .unwrap()
            .push(format!("askuser:{request_id}"));
        self.resolved.notify_one();
    }

    fn respond_permission(&self, request_id: &str, allow: bool) {
        self.answers
            .lock()
            .unwrap()
            .push(format!("permission:{request_id}:{allow}"));
    }

    fn respond_schedule(
        &self,
        request_id: &str,
        accepted: bool,
        _draft: Option<ScheduledPromptDraft>,
    ) {
        self.schedule_requests.lock().unwrap().remove(request_id);
        self.answers
            .lock()
            .unwrap()
            .push(format!("schedule:{request_id}:{accepted}"));
    }

    fn has_pending_schedule(&self, request_id: &str) -> bool {
        self.schedule_requests.lock().unwrap().contains(request_id)
    }
}

#[async_trait]
impl ToolHost for StubToolHost {
    async fn installed_extension_tools(&self) -> Vec<Tool> {
        // Sorted by id, because the cap takes a PREFIX of this list and a
        // `HashMap`'s iteration order would make every cap assertion a coin
        // flip. The real catalog is a stable query result.
        let mut tools: Vec<Tool> = self
            .script
            .lock()
            .unwrap()
            .tools
            .values()
            .cloned()
            .collect();
        tools.sort_by(|a, b| a.id.cmp(&b.id));
        tools
    }

    async fn get_tool(&self, tool_id: &str) -> Option<Tool> {
        self.script.lock().unwrap().tools.get(tool_id).cloned()
    }

    async fn list_mcp_tools(&self) -> Vec<Tool> {
        self.script.lock().unwrap().mcp_tools.clone()
    }

    async fn is_mcp_server(&self, id: &str) -> bool {
        self.script.lock().unwrap().mcp_servers.contains(id)
    }

    async fn load_tool_embeddings(&self) -> HashMap<String, super::deps::ToolEmbedding> {
        HashMap::new()
    }

    async fn filter_phase1(
        &self,
        _query_vector: &[f32],
        _options: super::deps::Phase1Options,
    ) -> super::deps::Phase1Result {
        super::deps::Phase1Result::default()
    }

    async fn filter_phase2(
        &self,
        _query: &str,
        candidates: Vec<crate::contract::api::ToolDescriptor>,
        _endpoint: &crate::llm::LlmEndpointConfig,
        _reasoning_budget: u32,
    ) -> Vec<crate::contract::api::ToolDescriptor> {
        candidates
    }

    async fn verify_tool_fresh(&self, _extension_id: &str) -> crate::error::Result<()> {
        match self.script.lock().unwrap().verify_error.clone() {
            Some(error) => Err(AppError::new(ErrorCode::InternalError, error)),
            None => Ok(()),
        }
    }

    fn validate_tool_args(
        &self,
        _tool: &Tool,
        arguments_json: &str,
    ) -> crate::error::Result<String> {
        match self.script.lock().unwrap().validate_error.clone() {
            Some(error) => Err(AppError::new(ErrorCode::ValidationError, error)),
            None => Ok(arguments_json.to_string()),
        }
    }

    async fn start_tool_call(
        &self,
        _spec: ToolCallSpec,
        events: mpsc::UnboundedSender<ToolCallEvent>,
    ) -> crate::error::Result<Box<dyn ToolCallController>> {
        let (outcome, cancel_resolves, scripted, start_error) = {
            let mut script = self.script.lock().unwrap();
            (
                script.outcome.take(),
                script.cancel_resolves,
                std::mem::take(&mut script.events),
                script.start_error.clone(),
            )
        };
        if let Some(error) = start_error {
            return Err(AppError::new(ErrorCode::ServerUnavailable, error));
        }
        let mut pending_schedules = HashSet::new();
        for event in &scripted {
            if let ToolCallEvent::ScheduleRequest { request_id, .. } = event {
                pending_schedules.insert(request_id.clone());
            }
        }
        for event in scripted {
            let _ = events.send(event);
        }
        self.starts.fetch_add(1, Ordering::Relaxed);
        self.started.notify_waiters();
        Ok(Box::new(StubController {
            outcome: Mutex::new(outcome),
            resolved: Arc::new(tokio::sync::Notify::new()),
            cancels: Arc::clone(&self.cancels),
            cancel_resolves,
            schedule_requests: Mutex::new(pending_schedules),
            answers: Arc::new(Mutex::new(Vec::new())),
        }))
    }

    async fn call_mcp_tool(
        &self,
        _server_id: &str,
        _name: &str,
        _args: Map<String, Value>,
    ) -> crate::error::Result<Value> {
        let scripted = self.script.lock().unwrap().mcp_result.clone();
        match scripted {
            Some(Ok(value)) => Ok(value),
            Some(Err(error)) => Err(AppError::new(ErrorCode::InternalError, error)),
            // Nothing scripted: hang, so a test that forgot to script it fails
            // by timing out rather than by silently taking a success path.
            None => std::future::pending().await,
        }
    }

    async fn resolve_mcp_tokens(&self, _text: &str) -> super::deps::McpTokens {
        super::deps::McpTokens::default()
    }
}

struct DispatchFixture {
    _tmp: TempDir,
    store: SessionsStore,
    frames: Arc<FrameBus>,
    _streams: Arc<ActiveStreams>,
    guard: StreamGuard,
    writer: TurnWriter,
    conn: Connection,
    dispatcher: Dispatcher,
    host: Arc<StubToolHost>,
    in_flight: Arc<InFlight>,
}

async fn dispatch_fixture(script: Script) -> DispatchFixture {
    let (tmp, store) = store();
    let frames = bus();
    let streams = streams();
    let session = new_session(&store, ALICE).await;
    let anchor = store
        .insert_message_after(&session, user_message("do the thing"), None)
        .await
        .unwrap()
        .id;
    let guard = claim(&streams, "s1", &session, ALICE);
    let mut conn = frames.register(ALICE);
    let _ = drain(&mut conn);
    let writer = TurnWriter::new(
        Arc::clone(guard.stream()),
        store.clone(),
        Arc::clone(&frames),
        Some(anchor),
    );
    let host = StubToolHost::new(script);
    let in_flight = Arc::new(InFlight::new());
    let dispatcher = Dispatcher::new(
        Arc::clone(&host) as Arc<dyn ToolHost>,
        Arc::clone(&frames),
        store.clone(),
        Arc::clone(&in_flight),
    );
    DispatchFixture {
        _tmp: tmp,
        store,
        frames,
        _streams: streams,
        guard,
        writer,
        conn,
        dispatcher,
        host,
        in_flight,
    }
}

fn pending_call(extension_id: &str, tool_name: &str) -> PendingCall {
    PendingCall {
        call_id: "s1:0:c1".into(),
        extension_id: extension_id.into(),
        tool_name: tool_name.into(),
        arguments: "{}".into(),
        unknown: false,
    }
}

fn tool_message() -> LiveRef {
    let Value::Object(map) = json!({
        "id": "m-tool",
        "ord": -1,
        "role": "tool",
        "callId": "s1:0:c1",
        "extensionId": "ext",
        "toolName": "do_thing",
        "arguments": "{}",
        "status": "running",
        "createdAtMs": 1_700_000_000_000i64,
    }) else {
        unreachable!()
    };
    Arc::new(Mutex::new(StoredMessage::new(map)))
}

fn field(message: &LiveRef, key: &str) -> Value {
    message
        .lock()
        .unwrap()
        .as_map()
        .get(key)
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn an_unknown_tool_name_fails_the_message_and_emits_no_frame() {
    let mut f = dispatch_fixture(Script::default()).await;
    let mut call = pending_call("unknown", "ghost");
    call.unknown = true;
    let message = tool_message();

    let displays = f
        .dispatcher
        .execute(f.guard.stream(), &call, &message, &mut f.writer)
        .await;

    assert!(displays.is_empty());
    assert_eq!(field(&message, "status"), json!("failed"));
    assert_eq!(field(&message, "error"), json!("tool ghost not available"));
    // The model learns why on the next hop; the client learns from the tool
    // message's own terminal snapshot, which the caller emits.
    assert!(drain(&mut f.conn).is_empty());
}

#[tokio::test]
async fn a_tool_id_that_resolves_to_nothing_fails_the_message() {
    // The catalog and the dispatcher disagreeing is the case: the muxer resolved
    // the name, and by dispatch time the tool is gone.
    let mut f = dispatch_fixture(Script::default()).await;
    let message = tool_message();

    f.dispatcher
        .execute(
            f.guard.stream(),
            &pending_call("ext", "do_thing"),
            &message,
            &mut f.writer,
        )
        .await;

    assert_eq!(field(&message, "status"), json!("failed"));
    assert_eq!(field(&message, "error"), json!("tool not found"));
}

#[tokio::test]
async fn an_mcp_server_error_fails_the_message_and_emits_no_tool_error_frame() {
    let mut f = dispatch_fixture(Script {
        mcp_servers: HashSet::from(["srv".to_string()]),
        mcp_result: Some(Err("the server refused".into())),
        ..Default::default()
    })
    .await;
    let message = tool_message();

    f.dispatcher
        .execute(
            f.guard.stream(),
            &pending_call("srv", "remote_thing"),
            &message,
            &mut f.writer,
        )
        .await;

    assert_eq!(field(&message, "status"), json!("failed"));
    assert!(
        field(&message, "error")
            .as_str()
            .unwrap_or_default()
            .contains("the server refused"),
        "{:?}",
        field(&message, "error")
    );
    // Deliberate asymmetry: only the extension path emits `tool.error`. The
    // instrument is the extension test below, where the recorder DOES see one.
    assert!(drain(&mut f.conn).is_empty());
}

#[tokio::test]
async fn an_mcp_is_error_result_becomes_the_failure_text() {
    let mut f = dispatch_fixture(Script {
        mcp_servers: HashSet::from(["srv".to_string()]),
        mcp_result: Some(Ok(json!({
            "isError": true,
            "content": [{ "type": "text", "text": "no such path" }]
        }))),
        ..Default::default()
    })
    .await;
    let message = tool_message();

    f.dispatcher
        .execute(
            f.guard.stream(),
            &pending_call("srv", "remote_thing"),
            &message,
            &mut f.writer,
        )
        .await;

    assert_eq!(field(&message, "status"), json!("failed"));
    assert_eq!(field(&message, "error"), json!("no such path"));
}

#[tokio::test]
async fn an_mcp_success_joins_the_text_parts_and_falls_back_to_the_raw_result() {
    let mut f = dispatch_fixture(Script {
        mcp_servers: HashSet::from(["srv".to_string()]),
        mcp_result: Some(Ok(json!({
            "content": [
                { "type": "text", "text": "first" },
                { "type": "image", "data": "ignored" },
                { "type": "text", "text": "second" }
            ]
        }))),
        ..Default::default()
    })
    .await;
    let message = tool_message();
    f.dispatcher
        .execute(
            f.guard.stream(),
            &pending_call("srv", "remote_thing"),
            &message,
            &mut f.writer,
        )
        .await;
    assert_eq!(field(&message, "status"), json!("completed"));
    assert_eq!(field(&message, "result"), json!("first\nsecond"));

    // No text at all: the raw result rather than an empty string, so a tool
    // answering with structured content only is not silently blanked.
    let mut g = dispatch_fixture(Script {
        mcp_servers: HashSet::from(["srv".to_string()]),
        mcp_result: Some(Ok(json!({ "structuredContent": { "rows": 2 } }))),
        ..Default::default()
    })
    .await;
    let message = tool_message();
    g.dispatcher
        .execute(
            g.guard.stream(),
            &pending_call("srv", "remote_thing"),
            &message,
            &mut g.writer,
        )
        .await;
    assert_eq!(
        field(&message, "result"),
        json!({ "structuredContent": { "rows": 2 } })
    );
}

#[tokio::test]
async fn a_content_hash_drift_fails_the_call_and_does_emit_a_tool_error_frame() {
    let mut f = dispatch_fixture(Script {
        tools: HashMap::from([("ext::do_thing".to_string(), stub_tool("ext", "do_thing"))]),
        verify_error: Some("extension content changed since install".into()),
        ..Default::default()
    })
    .await;
    let message = tool_message();

    f.dispatcher
        .execute(
            f.guard.stream(),
            &pending_call("ext", "do_thing"),
            &message,
            &mut f.writer,
        )
        .await;

    assert_eq!(field(&message, "status"), json!("failed"));
    // This is the instrument the two MCP tests above lean on: the same recorder,
    // the same drain, and here it DOES see a frame.
    let sent = drain(&mut f.conn);
    assert_eq!(kinds(&sent), vec!["tool.error"]);
    assert_eq!(sent[0]["callId"], json!("s1:0:c1"));
    assert_eq!(f.host.starts.load(Ordering::Relaxed), 0, "never dispatched");
}

#[tokio::test]
async fn arguments_the_schema_rejects_fail_the_call_before_the_worker_starts() {
    let mut f = dispatch_fixture(Script {
        tools: HashMap::from([("ext::do_thing".to_string(), stub_tool("ext", "do_thing"))]),
        validate_error: Some("path is required".into()),
        ..Default::default()
    })
    .await;
    let message = tool_message();

    f.dispatcher
        .execute(
            f.guard.stream(),
            &pending_call("ext", "do_thing"),
            &message,
            &mut f.writer,
        )
        .await;

    assert_eq!(field(&message, "status"), json!("failed"));
    assert_eq!(field(&message, "error"), json!("path is required"));
    assert_eq!(kinds(&drain(&mut f.conn)), vec!["tool.error"]);
    assert_eq!(f.host.starts.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn a_worker_that_resolves_completes_the_message_and_sends_the_result() {
    let mut f = dispatch_fixture(Script {
        tools: HashMap::from([("ext::do_thing".to_string(), stub_tool("ext", "do_thing"))]),
        outcome: Some(Ok(json!({ "rows": 3 }))),
        ..Default::default()
    })
    .await;
    let message = tool_message();

    f.dispatcher
        .execute(
            f.guard.stream(),
            &pending_call("ext", "do_thing"),
            &message,
            &mut f.writer,
        )
        .await;

    assert_eq!(field(&message, "status"), json!("completed"));
    assert_eq!(field(&message, "result"), json!({ "rows": 3 }));
    let sent = drain(&mut f.conn);
    assert_eq!(kinds(&sent), vec!["tool.result"]);
    assert_eq!(sent[0]["result"], json!({ "rows": 3 }));
}

#[tokio::test]
async fn a_worker_that_rejects_fails_the_message_and_sends_tool_error() {
    let mut f = dispatch_fixture(Script {
        tools: HashMap::from([("ext::do_thing".to_string(), stub_tool("ext", "do_thing"))]),
        outcome: Some(Err("the tool threw".into())),
        ..Default::default()
    })
    .await;
    let message = tool_message();

    f.dispatcher
        .execute(
            f.guard.stream(),
            &pending_call("ext", "do_thing"),
            &message,
            &mut f.writer,
        )
        .await;

    assert_eq!(field(&message, "status"), json!("failed"));
    assert!(field(&message, "error")
        .as_str()
        .unwrap_or_default()
        .contains("the tool threw"));
    assert_eq!(kinds(&drain(&mut f.conn)), vec!["tool.error"]);
}

#[tokio::test]
async fn progress_is_persisted_on_the_message_and_forwarded() {
    let mut f = dispatch_fixture(Script {
        tools: HashMap::from([("ext::do_thing".to_string(), stub_tool("ext", "do_thing"))]),
        events: vec![ToolCallEvent::Progress {
            progress: 0.5,
            label: Some("Opening YouTube".into()),
            description: Some("almost there".into()),
        }],
        outcome: Some(Ok(Value::Null)),
        ..Default::default()
    })
    .await;
    let message = tool_message();

    f.dispatcher
        .execute(
            f.guard.stream(),
            &pending_call("ext", "do_thing"),
            &message,
            &mut f.writer,
        )
        .await;

    // Persisted so a reloaded bubble keeps the tool's own wording rather than
    // falling back to the generic tool-name phrase.
    assert_eq!(field(&message, "label"), json!("Opening YouTube"));
    assert_eq!(field(&message, "description"), json!("almost there"));
    assert_eq!(field(&message, "progress"), json!(0.5));
    assert_eq!(
        kinds(&drain(&mut f.conn)),
        vec!["tool.progress", "tool.result"]
    );
}

#[tokio::test]
async fn display_pushes_are_born_but_left_unfinalized_for_the_caller() {
    let mut f = dispatch_fixture(Script {
        tools: HashMap::from([("ext::do_thing".to_string(), stub_tool("ext", "do_thing"))]),
        events: vec![
            ToolCallEvent::Display {
                content: DisplayContent::Markdown {
                    markdown: "# one".into(),
                },
            },
            ToolCallEvent::Display {
                content: DisplayContent::Markdown {
                    markdown: "# two".into(),
                },
            },
        ],
        outcome: Some(Ok(Value::Null)),
        ..Default::default()
    })
    .await;
    let message = tool_message();

    let displays = f
        .dispatcher
        .execute(
            f.guard.stream(),
            &pending_call("ext", "do_thing"),
            &message,
            &mut f.writer,
        )
        .await;

    assert_eq!(displays.len(), 2);
    // Born, so the client sees them in order behind the running tool message.
    let sent = kinds(&drain(&mut f.conn));
    assert_eq!(
        sent,
        vec!["chat.message", "chat.message", "tool.result"],
        "{sent:?}"
    );
    // NOT finalized: persisting them here would put them ahead of the still
    // running tool message, so the durable order would contradict the live one.
    assert!(
        f.store
            .list_messages(&f.guard.stream().session_id)
            .await
            .iter()
            .all(|m| m.role() != Some("display")),
        "displays are the caller's to persist"
    );
}

#[tokio::test]
async fn a_prompt_is_retained_for_catch_up_and_cleared_when_the_call_ends() {
    let mut f = dispatch_fixture(Script {
        tools: HashMap::from([("ext::do_thing".to_string(), stub_tool("ext", "do_thing"))]),
        events: vec![ToolCallEvent::AskUserRequest {
            request_id: "req-1".into(),
            questions: Vec::new(),
        }],
        outcome: None,
        cancel_resolves: true,
        ..Default::default()
    })
    .await;
    let message = tool_message();
    let stream = Arc::clone(f.guard.stream());
    let mut writer = f.writer;
    let dispatcher = f.dispatcher;
    let call = pending_call("ext", "do_thing");
    let task = tokio::spawn({
        let stream = Arc::clone(&stream);
        let message = Arc::clone(&message);
        async move {
            dispatcher
                .execute(&stream, &call, &message, &mut writer)
                .await;
        }
    });

    // The prompt is retained while the call is open, so a client that
    // reconnects mid-call is re-asked rather than left waiting.
    while stream.outstanding_prompts().is_empty() {
        tokio::task::yield_now().await;
    }
    stream.cancel.cancel();
    task.await.unwrap();

    assert!(
        stream.outstanding_prompts().is_empty(),
        "an answered or ended call leaves no prompt to replay"
    );
    let sent = kinds(&drain(&mut f.conn));
    assert!(
        sent.contains(&"tool.askuser_request".to_string()),
        "{sent:?}"
    );
}

#[tokio::test]
async fn an_interrupt_cancels_the_controller_and_the_message_still_gets_an_outcome() {
    let f = dispatch_fixture(Script {
        tools: HashMap::from([("ext::do_thing".to_string(), stub_tool("ext", "do_thing"))]),
        outcome: None,
        cancel_resolves: true,
        ..Default::default()
    })
    .await;
    let message = tool_message();
    let stream = Arc::clone(f.guard.stream());
    let host = Arc::clone(&f.host);
    let mut writer = f.writer;
    let dispatcher = f.dispatcher;
    let call = pending_call("ext", "do_thing");
    let task = tokio::spawn({
        let stream = Arc::clone(&stream);
        let message = Arc::clone(&message);
        async move {
            dispatcher
                .execute(&stream, &call, &message, &mut writer)
                .await;
        }
    });

    while host.starts.load(Ordering::Relaxed) == 0 {
        tokio::task::yield_now().await;
    }
    stream.cancel.cancel();
    task.await.unwrap();

    assert_eq!(host.cancels.load(Ordering::Relaxed), 1);
    // The dispatcher keeps awaiting the controller after cancelling, so the
    // worker's own cancel path runs and the message ends in a real state rather
    // than staying `running` forever.
    assert_eq!(field(&message, "status"), json!("failed"));
    assert!(f.in_flight.owned("s1:0:c1", ALICE).is_none());
}

#[tokio::test]
async fn an_already_interrupted_turn_cancels_the_call_before_it_can_run() {
    // The pre-check: the turn is over, and without it the worker runs to
    // completion with nobody waiting for the answer.
    let mut f = dispatch_fixture(Script {
        tools: HashMap::from([("ext::do_thing".to_string(), stub_tool("ext", "do_thing"))]),
        outcome: Some(Ok(Value::Null)),
        ..Default::default()
    })
    .await;
    let message = tool_message();
    f.guard.stream().cancel.cancel();

    f.dispatcher
        .execute(
            f.guard.stream(),
            &pending_call("ext", "do_thing"),
            &message,
            &mut f.writer,
        )
        .await;

    assert_eq!(f.host.cancels.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn an_in_flight_call_is_addressable_only_by_the_client_that_started_it() {
    let mut f = dispatch_fixture(Script {
        tools: HashMap::from([("ext::do_thing".to_string(), stub_tool("ext", "do_thing"))]),
        outcome: None,
        cancel_resolves: true,
        ..Default::default()
    })
    .await;
    let message = tool_message();
    let stream = Arc::clone(f.guard.stream());
    let host = Arc::clone(&f.host);
    let in_flight = Arc::clone(&f.in_flight);
    let mut writer = f.writer;
    let dispatcher = f.dispatcher;
    let call = pending_call("ext", "do_thing");
    let task = tokio::spawn({
        let stream = Arc::clone(&stream);
        let message = Arc::clone(&message);
        async move {
            dispatcher
                .execute(&stream, &call, &message, &mut writer)
                .await;
        }
    });
    while host.starts.load(Ordering::Relaxed) == 0 {
        tokio::task::yield_now().await;
    }

    // A collision in this map is what makes an askUser answer land on someone
    // else's tool call, so the owner check is the guard, not the call id alone.
    assert!(in_flight.owned("s1:0:c1", BOB).is_none());
    assert!(in_flight.owned_stream("s1:0:c1", BOB).is_none());
    assert!(in_flight.owned("s1:0:c1", ALICE).is_some());
    assert!(in_flight.owned_stream("s1:0:c1", ALICE).is_some());

    stream.cancel.cancel();
    task.await.unwrap();
    let _ = f.frames;
    let _ = drain(&mut f.conn);
}

// --- complexity.rs -----------------------------------------------------------

use super::complexity::{classify_complexity, route_of};

#[test]
fn the_classifier_reads_complex_only_when_simple_is_absent() {
    // A table: the "both words present" row is the whole reason the predicate is
    // a conjunction, and a single "complex" case passes without it.
    let cases = [
        ("complex", LlmRoute::Secondary),
        ("COMPLEX", LlmRoute::Secondary),
        ("This looks complex to me.", LlmRoute::Secondary),
        ("simple", LlmRoute::Default),
        ("not complex, simple", LlmRoute::Default),
        ("complex or simple?", LlmRoute::Default),
        ("", LlmRoute::Default),
        ("banana", LlmRoute::Default),
    ];
    for (answer, expected) in cases {
        assert_eq!(route_of(answer), expected, "answer {answer:?}");
    }
}

/// An in-memory database WITH the schema applied. `open_in_memory` deliberately
/// skips the migration, so a fixture that forgets this gets a store whose every
/// read fails, which surfaces two layers away as a turn that errored.
fn migrated_db() -> crate::db::Db {
    let db = crate::db::Db::open_in_memory().unwrap();
    db.call_blocking(|conn| crate::db::migrate(conn)).unwrap();
    db
}

/// A `Settings` + `Vault` + `Llm` triple over a tempdir, with the provider
/// scripted. Enough to drive anything that resolves an endpoint.
struct LlmEnv {
    _tmp: TempDir,
    llm: crate::llm::Llm,
}

fn llm_env(provider: Arc<dyn crate::llm::ChatProvider>) -> LlmEnv {
    let tmp = tempfile::tempdir().unwrap();
    // No `Settings` handle: these callers pass the record in directly, and the
    // vault is only here because `Llm` resolves external keys through it.
    let vault = crate::secrets::Vault::with_store(
        tmp.path().join("secrets.enc"),
        Arc::new(crate::secrets::testing::MemoryStore::new()),
    );
    let llm = crate::llm::Llm::with_provider(vault, crate::config::Channel::Stable, provider);
    LlmEnv { _tmp: tmp, llm }
}

/// A local provider, which is what an unconfigured core resolves to: a loopback
/// URL plus a placeholder key and model, so the endpoint gate passes.
fn local_settings() -> SettingsRecord {
    let Value::Object(map) = json!({ "llm.provider": "local" }) else {
        unreachable!()
    };
    map
}

#[tokio::test]
async fn the_classifier_routes_a_complex_answer_to_the_secondary_model() {
    let env = llm_env(Arc::new(crate::llm::testing::ScriptedProvider::text(
        "complex",
    )));
    let settings = local_settings();

    let route = classify_complexity(
        &env.llm,
        &settings,
        &settings,
        "rewrite my compiler",
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    assert_eq!(route, LlmRoute::Secondary);
}

#[tokio::test]
async fn a_failing_classifier_falls_back_to_the_default_model() {
    // The bias is deliberate: the failure that costs the user money is the one
    // to guard against, so an unusable classifier must not route to the
    // secondary endpoint.
    let env = llm_env(Arc::new(crate::llm::testing::ScriptedProvider::failing(
        crate::llm::ProviderError::transport("upstream is down"),
    )));
    let settings = local_settings();

    let route = classify_complexity(
        &env.llm,
        &settings,
        &settings,
        "rewrite my compiler",
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    assert_eq!(route, LlmRoute::Default);
}

#[tokio::test]
async fn a_cancelled_classifier_returns_without_waiting_for_the_model() {
    // A provider that never produces anything, so the only way out is the
    // cancel arm. Without it the turn would sit here after an interrupt.
    let env = llm_env(Arc::new(
        crate::llm::testing::ScriptedProvider::text("complex")
            .with_gap(std::time::Duration::from_secs(3600)),
    ));
    let settings = local_settings();
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();

    let route = classify_complexity(
        &env.llm,
        &settings,
        &settings,
        "rewrite my compiler",
        &cancel,
    )
    .await;

    assert_eq!(route, LlmRoute::Default);
}

// --- title.rs ----------------------------------------------------------------

use super::title::{sanitize, select_title_context};

#[test]
fn sanitize_applies_its_six_steps_in_order() {
    // The ORDER is the content of the function: each row pins one step against
    // the one before it.
    let cases = [
        ("Hello world", "Hello world"),
        // The think block goes first: taking the first line first would leave a
        // dangling `<think>` opener behind.
        ("<think>hmm</think>Refactor plan", "Refactor plan"),
        // The consequence of that order: a block followed by a newline leaves
        // the first line empty, so the session stays untitled.
        ("<think>\nhmm\n</think>\nRefactor plan", ""),
        ("<THINK>x</THINK>Caps tag", "Caps tag"),
        // Only the first line survives.
        ("A title\nand commentary", "A title"),
        ("Title: Deploy notes", "Deploy notes"),
        ("title - Deploy notes", "Deploy notes"),
        // Quotes come off before the trailing punctuation, so both go.
        ("\"Quoted title\"", "Quoted title"),
        ("Ends in punctuation!!!", "Ends in punctuation"),
        ("`Backticked`", "Backticked"),
        ("  spaced   out   words ", "spaced out words"),
        ("", ""),
    ];
    for (raw, expected) in cases {
        assert_eq!(sanitize(raw), expected, "raw {raw:?}");
    }
}

#[test]
fn a_runaway_title_is_capped_at_eighty_characters() {
    // The fixture is a literal 120, not `MAX_TITLE_CHARS + 40`: a test written
    // in terms of the constant it tests cannot see that constant change.
    let raw = "x".repeat(120);

    let title = sanitize(&raw);

    assert_eq!(title.chars().count(), 80);
}

fn turn_message(role: &str, text: &str) -> StoredMessage {
    let Value::Object(map) = json!({
        "id": ulid::Ulid::new().to_string(),
        "ord": 0,
        "role": role,
        "content": text,
        "createdAtMs": 1_700_000_000_000i64,
    }) else {
        unreachable!()
    };
    StoredMessage::new(map)
}

#[test]
fn the_opening_turn_is_shown_separately_and_left_out_of_the_recent_walk() {
    let history = vec![
        turn_message("user", "opening question"),
        turn_message("assistant", "first answer"),
        turn_message("user", "follow up"),
    ];

    let context = select_title_context(&history, "{firstMessage}{recentMessages}", 4096, 32);

    assert_eq!(context.first_message, "opening question");
    // Counted once. Including it in the walk too would spend the budget on text
    // the template already carries.
    assert!(!context.recent_messages.contains("opening question"));
    assert_eq!(
        context.recent_messages,
        "Assistant: first answer\nUser: follow up",
    );
}

#[test]
fn the_bubbles_the_title_prompt_cannot_use_are_skipped() {
    let history = vec![
        turn_message("user", "opening question"),
        turn_message("reasoning", "thinking out loud"),
        turn_message("tool", "{}"),
        turn_message("assistant", ""),
        turn_message("assistant", "real answer"),
    ];

    let context = select_title_context(&history, "{firstMessage}{recentMessages}", 4096, 32);

    assert_eq!(context.recent_messages, "Assistant: real answer");
}

#[test]
fn a_tight_context_window_keeps_the_newest_turns_and_drops_the_oldest() {
    // The middle line is sized to fall BETWEEN the budget with the safety margin
    // subtracted and the budget without it: about 52 tokens, against 26
    // remaining with the margin and 90 without.
    let history = vec![
        turn_message("user", "opening question"),
        turn_message("assistant", &"a".repeat(200)),
        turn_message("user", "newest"),
    ];

    let context = select_title_context(&history, "{firstMessage}{recentMessages}", 130, 32);

    assert_eq!(context.recent_messages, "User: newest");
}

#[test]
fn a_transcript_with_no_conversational_turns_produces_nothing() {
    let history = vec![turn_message("tool", "{}"), turn_message("display", "x")];

    let context = select_title_context(&history, "{firstMessage}{recentMessages}", 4096, 32);

    assert_eq!(context.first_message, "");
    assert_eq!(context.recent_messages, "");
}

// --- background.rs -----------------------------------------------------------

use super::background::{BackgroundJob, BackgroundQueue};

/// A queue whose gate a test drives, plus a channel recording each key as its
/// job STARTS. Recording at the start rather than at the end is what makes the
/// single-active and key-freeing assertions observable.
struct QueueEnv {
    queue: Arc<BackgroundQueue>,
    busy: Arc<std::sync::atomic::AtomicBool>,
    ran: tokio::sync::mpsc::UnboundedReceiver<String>,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
}

fn queue_env(quiet: std::time::Duration) -> QueueEnv {
    let busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gate = Arc::clone(&busy);
    let queue = BackgroundQueue::with_timings(
        Arc::new(move || gate.load(Ordering::Relaxed)),
        quiet,
        std::time::Duration::from_millis(50),
    );
    let (tx, ran) = tokio::sync::mpsc::unbounded_channel();
    QueueEnv {
        queue,
        busy,
        ran,
        tx,
    }
}

/// The next job to run, or `None` if none runs within the budget.
///
/// Every wait in these tests goes through this. An unadorned `recv().await` is
/// unbounded, and on a paused runtime with no pending timer there is nothing to
/// auto-advance to, so a mutation that stops the drain HANGS the test instead of
/// failing it: the suite then reports nothing at all rather than a red. The
/// budget is virtual time, so this costs nothing when the job does arrive.
async fn next_run(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Option<String> {
    tokio::time::timeout(std::time::Duration::from_secs(120), rx.recv())
        .await
        .ok()
        .flatten()
}

fn recording_job(key: &str, tx: tokio::sync::mpsc::UnboundedSender<String>) -> BackgroundJob {
    let key_for_run = key.to_string();
    BackgroundJob::new(key, move || {
        Box::pin(async move {
            let _ = tx.send(key_for_run);
            Ok(())
        })
    })
}

#[tokio::test(start_paused = true)]
async fn a_key_already_queued_is_not_enqueued_again() {
    let mut env = queue_env(std::time::Duration::ZERO);
    env.busy.store(true, Ordering::Relaxed);

    env.queue.enqueue(recording_job("k", env.tx.clone()));
    env.queue.enqueue(recording_job("k", env.tx.clone()));

    assert_eq!(env.queue.len(), 1);
    // Paired with the instrument check: with the gate open the queue really
    // does run what it holds, so "one entry" is not a queue that never works.
    env.busy.store(false, Ordering::Relaxed);
    assert_eq!(next_run(&mut env.ran).await.as_deref(), Some("k"));
    assert!(env.ran.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn jobs_drain_in_the_order_they_were_queued() {
    let mut env = queue_env(std::time::Duration::ZERO);
    env.busy.store(true, Ordering::Relaxed);
    for key in ["a", "b", "c"] {
        env.queue.enqueue(recording_job(key, env.tx.clone()));
    }

    env.busy.store(false, Ordering::Relaxed);

    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(next_run(&mut env.ran).await.expect("the drain stopped"));
    }
    assert_eq!(seen, vec!["a", "b", "c"]);
}

#[tokio::test(start_paused = true)]
async fn nothing_drains_while_chat_is_busy() {
    let mut env = queue_env(std::time::Duration::ZERO);
    env.busy.store(true, Ordering::Relaxed);
    env.queue.enqueue(recording_job("k", env.tx.clone()));

    // The gate is not time-based, so auto-advancing the paused clock cannot
    // open it: this really is "held back", not "not yet reached".
    tokio::time::advance(std::time::Duration::from_secs(600)).await;
    tokio::task::yield_now().await;
    assert!(env.ran.try_recv().is_err());

    env.busy.store(false, Ordering::Relaxed);
    assert_eq!(next_run(&mut env.ran).await.as_deref(), Some("k"));
}

#[tokio::test(start_paused = true)]
async fn the_first_job_waits_out_one_quiet_period_after_boot() {
    let quiet = std::time::Duration::from_secs(5);
    let mut env = queue_env(quiet);
    let started = tokio::time::Instant::now();

    env.queue.enqueue(recording_job("k", env.tx.clone()));
    next_run(&mut env.ran)
        .await
        .expect("the first job never ran");

    // Measured on tokio's clock, which the paused runtime advances by exactly
    // what the sleep asked for. A quiet period of zero makes this ~0 and fails.
    assert!(
        started.elapsed() >= quiet,
        "ran after {:?}, expected at least {quiet:?}",
        started.elapsed(),
    );
}

#[tokio::test(start_paused = true)]
async fn only_one_job_runs_at_a_time() {
    let env = queue_env(std::time::Duration::ZERO);
    let release = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let second_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let (r, e) = (Arc::clone(&release), Arc::clone(&entered));
    env.queue.enqueue(BackgroundJob::new("slow", move || {
        Box::pin(async move {
            e.notify_waiters();
            r.notified().await;
            Ok(())
        })
    }));
    let flag = Arc::clone(&second_ran);
    env.queue.enqueue(BackgroundJob::new("fast", move || {
        Box::pin(async move {
            flag.store(true, Ordering::Relaxed);
            Ok(())
        })
    }));

    let wait = entered.notified();
    tokio::pin!(wait);
    tokio::time::timeout(std::time::Duration::from_secs(120), wait)
        .await
        .expect("the first job never started");
    tokio::task::yield_now().await;
    assert!(!second_ran.load(Ordering::Relaxed));

    release.notify_waiters();
    for _ in 0..10_000 {
        if second_ran.load(Ordering::Relaxed) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        second_ran.load(Ordering::Relaxed),
        "the drain never resumed"
    );
}

#[tokio::test(start_paused = true)]
async fn the_key_frees_when_the_job_starts_so_a_re_enqueue_runs_again() {
    let mut env = queue_env(std::time::Duration::ZERO);
    let release = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let (r, e) = (Arc::clone(&release), Arc::clone(&entered));
    env.queue.enqueue(BackgroundJob::new("k", move || {
        Box::pin(async move {
            e.notify_waiters();
            r.notified().await;
            Ok(())
        })
    }));
    let wait = entered.notified();
    tokio::pin!(wait);
    tokio::time::timeout(std::time::Duration::from_secs(120), wait)
        .await
        .expect("the job never started");

    // The source changed while the pass was running, so a fresh pass must be
    // queued rather than swallowed as a duplicate of the running one.
    env.queue.enqueue(recording_job("k", env.tx.clone()));
    release.notify_waiters();

    assert_eq!(next_run(&mut env.ran).await.as_deref(), Some("k"));
}

#[tokio::test(start_paused = true)]
async fn a_failing_job_does_not_stop_the_drain() {
    let mut env = queue_env(std::time::Duration::ZERO);
    env.busy.store(true, Ordering::Relaxed);
    env.queue.enqueue(BackgroundJob::new("bad", || {
        Box::pin(async {
            Err(crate::error::AppError::new(
                ErrorCode::InternalError,
                "no model",
            ))
        })
    }));
    env.queue.enqueue(recording_job("good", env.tx.clone()));

    env.busy.store(false, Ordering::Relaxed);

    assert_eq!(next_run(&mut env.ran).await.as_deref(), Some("good"));
}

#[tokio::test(start_paused = true)]
async fn dispose_drops_the_queue_and_stops_the_drain() {
    let mut env = queue_env(std::time::Duration::ZERO);
    env.busy.store(true, Ordering::Relaxed);
    env.queue.enqueue(recording_job("k", env.tx.clone()));

    env.queue.dispose();
    env.busy.store(false, Ordering::Relaxed);
    tokio::time::advance(std::time::Duration::from_secs(600)).await;
    tokio::task::yield_now().await;

    assert_eq!(env.queue.len(), 0);
    assert!(env.ran.try_recv().is_err());
    // And a job queued after dispose is refused rather than accumulating.
    env.queue.enqueue(recording_job("later", env.tx.clone()));
    assert_eq!(env.queue.len(), 0);
}

// --- tools/selection.rs ------------------------------------------------------

use crate::contract::api::{Grant, GrantState, PermissionDecl};

use super::tools::selection::{build_tool_list, tool_exposable, SelectionInputs};

/// An `Embedding` with no local model and no external relevance endpoint, which
/// is what every core has until one is configured. `embed` fails, which is the
/// state the "MCP survives an embedding failure" asymmetry exists for.
fn embedding_env() -> (TempDir, crate::memories::Embedding) {
    let tmp = tempfile::tempdir().unwrap();
    let db = migrated_db();
    let settings =
        crate::settings::Settings::with_file(tmp.path().join("settings.json"), db, bus());
    let vault = crate::secrets::Vault::with_store(
        tmp.path().join("secrets.enc"),
        Arc::new(crate::secrets::testing::MemoryStore::new()),
    );
    (tmp, crate::memories::Embedding::unwired(settings, vault))
}

fn settings_of(value: Value) -> SettingsRecord {
    let Value::Object(map) = value else {
        unreachable!()
    };
    map
}

async fn select(
    script: Script,
    settings: Value,
    query: Option<&str>,
) -> super::tools::selection::ToolSelection {
    let host = StubToolHost::new(script);
    let (_tmp, embedding) = embedding_env();
    let settings = settings_of(settings);
    let cancel = tokio_util::sync::CancellationToken::new();
    build_tool_list(
        host.as_ref(),
        &embedding,
        SelectionInputs {
            settings: &settings,
            endpoint: &crate::llm::LlmEndpointConfig::default(),
            stream_id: "s1",
            cancel: &cancel,
            query_text: query,
        },
    )
    .await
}

fn tool_names(selection: &super::tools::selection::ToolSelection) -> Vec<String> {
    selection
        .tools
        .iter()
        .map(|t| t.function.name.clone())
        .collect()
}

fn bubble_field(selection: &super::tools::selection::ToolSelection, key: &str) -> Value {
    selection
        .filter_message
        .as_ref()
        .map(|m| {
            m.lock()
                .unwrap()
                .as_map()
                .get(key)
                .cloned()
                .unwrap_or(Value::Null)
        })
        .unwrap_or(Value::Null)
}

fn catalog(tools: Vec<Tool>) -> HashMap<String, Tool> {
    tools.into_iter().map(|t| (t.id.clone(), t)).collect()
}

#[tokio::test]
async fn tools_off_produces_no_tools_and_no_bubble_at_all() {
    let script = Script {
        tools: catalog(vec![stub_tool("ext", "alpha")]),
        ..Default::default()
    };

    let selection = select(script, json!({ "tools.enabled": false }), Some("do it")).await;

    assert!(selection.tools.is_empty());
    // Not an empty bubble: a no-tools turn shows nothing rather than an empty
    // selection the user has to learn to ignore.
    assert!(selection.filter_message.is_none());
    assert!(selection.query_vector.is_none());
}

#[tokio::test]
async fn the_cap_applies_and_always_available_tools_are_exempt_from_it() {
    let mut always = stub_tool("ext", "zulu");
    always.always_available = true;
    let script = Script {
        tools: catalog(vec![
            stub_tool("ext", "alpha"),
            stub_tool("ext", "bravo"),
            always,
        ]),
        ..Default::default()
    };

    let selection = select(
        script,
        json!({
            "tools.enabled": true,
            "tools.filteringEnabled": false,
            "tools.maxTools": 1,
        }),
        Some("do it"),
    )
    .await;

    let names = tool_names(&selection);
    // One from the cap, plus the always-available one the cap cut off. Which of
    // alpha/bravo survives the cap is catalog order and is not asserted.
    assert_eq!(names.len(), 2, "got {names:?}");
    assert!(names.contains(&"zulu".to_string()), "got {names:?}");
}

#[tokio::test]
async fn a_denied_permission_withholds_a_tool_from_the_offering() {
    // Covers the FIRST of the two places the gate is applied, the catalog read.
    // The second, after the map back through the relevance index, needs a
    // working embedding endpoint these fixtures deliberately do not have.
    let mut denied = stub_tool("ext", "reader");
    denied.required_permissions = vec![PermissionDecl::Read {
        optional: None,
        path: "/etc".into(),
        reason: "reads config".into(),
    }];
    denied.grants = vec![Grant {
        granted_at_ms: 0,
        permission_key: "read:/etc".into(),
        permission_kind: PermissionKind::Read,
        state: GrantState::Denied,
        tool_id: denied.id.clone(),
    }];
    assert!(!tool_exposable(&denied));
    let script = Script {
        tools: catalog(vec![denied, stub_tool("ext", "harmless")]),
        ..Default::default()
    };

    let selection = select(
        script,
        json!({ "tools.enabled": true, "tools.filteringEnabled": false }),
        Some("read the config"),
    )
    .await;

    // Paired: the instrument really does let a tool through, so "reader absent"
    // is a withheld tool and not an empty pipeline.
    assert_eq!(tool_names(&selection), vec!["harmless".to_string()]);
}

#[tokio::test]
async fn an_optional_denied_permission_does_not_withhold_the_tool() {
    let mut tool = stub_tool("ext", "reader");
    tool.required_permissions = vec![PermissionDecl::Read {
        optional: Some(true),
        path: "/etc".into(),
        reason: "reads config if allowed".into(),
    }];
    tool.grants = vec![Grant {
        granted_at_ms: 0,
        permission_key: "read:/etc".into(),
        permission_kind: PermissionKind::Read,
        state: GrantState::Denied,
        tool_id: tool.id.clone(),
    }];

    assert!(tool_exposable(&tool));
}

#[tokio::test]
async fn an_ungranted_permission_behaves_as_ask_rather_than_as_denied() {
    // No grant row at all. The sandbox prompts at the moment of access, so
    // withholding the tool here would make an unanswered prompt silently shrink
    // the catalog.
    let mut tool = stub_tool("ext", "reader");
    tool.required_permissions = vec![PermissionDecl::Read {
        optional: None,
        path: "/etc".into(),
        reason: "reads config".into(),
    }];

    assert!(tool_exposable(&tool));
}

#[tokio::test]
async fn an_embedding_failure_errors_the_bubble_and_still_offers_mcp_tools() {
    // The extension pipeline is inside the failure guard and the MCP append is
    // outside it: MCP tools were never in the embedding index.
    let mut mcp = stub_tool("srv", "search_docs");
    mcp.always_available = true;
    let script = Script {
        tools: catalog(vec![stub_tool("ext", "alpha")]),
        mcp_tools: vec![mcp],
        ..Default::default()
    };

    let selection = select(
        script,
        json!({ "tools.enabled": true, "tools.filteringEnabled": true }),
        Some("find the docs"),
    )
    .await;

    assert_eq!(bubble_field(&selection, "status"), json!("error"));
    assert!(bubble_field(&selection, "errorMessage").is_string());
    assert_eq!(tool_names(&selection), vec!["search_docs".to_string()]);
}

#[tokio::test]
async fn tools_sent_counts_the_mcp_tools_appended_after_the_extension_pipeline() {
    // Counted after the append on purpose: an MCP-only turn would otherwise
    // report zero and suppress the system prompt's tools hint while the model
    // really did get tools.
    let script = Script {
        tools: HashMap::new(),
        mcp_tools: vec![stub_tool("srv", "search_docs")],
        ..Default::default()
    };

    let selection = select(
        script,
        json!({ "tools.enabled": true, "tools.filteringEnabled": false }),
        Some("find the docs"),
    )
    .await;

    assert_eq!(bubble_field(&selection, "toolsSent"), json!(1));
    assert_eq!(bubble_field(&selection, "mcp").as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn an_mcp_tool_whose_name_an_extension_claims_is_skipped() {
    // A duplicate function name confuses the model, and dispatch resolves the
    // name to the extension anyway, so the MCP entry would be dead weight.
    let script = Script {
        tools: catalog(vec![stub_tool("ext", "search_docs")]),
        mcp_tools: vec![stub_tool("srv", "search_docs")],
        ..Default::default()
    };

    let selection = select(
        script,
        json!({ "tools.enabled": true, "tools.filteringEnabled": false }),
        Some("find the docs"),
    )
    .await;

    assert_eq!(tool_names(&selection), vec!["search_docs".to_string()]);
    assert_eq!(bubble_field(&selection, "mcp"), json!([]));
}

#[tokio::test]
async fn the_mcp_append_is_bounded_by_max_tools() {
    let script = Script {
        tools: HashMap::new(),
        mcp_tools: (0..5)
            .map(|i| stub_tool("srv", &format!("tool_{i}")))
            .collect(),
        ..Default::default()
    };

    let selection = select(
        script,
        json!({
            "tools.enabled": true,
            "tools.filteringEnabled": false,
            "tools.maxTools": 2,
        }),
        Some("do it"),
    )
    .await;

    assert_eq!(selection.tools.len(), 2);
}

// --- turn.rs and the Chat handle ---------------------------------------------

use super::{Chat, ChatConfig, ChatDeps};

struct ChatEnv {
    _tmp: TempDir,
    chat: Chat,
    store: SessionsStore,
    frames: Arc<FrameBus>,
    settings: crate::settings::Settings,
}

async fn chat_env(provider: Arc<dyn crate::llm::ChatProvider>, deps: ChatDeps) -> ChatEnv {
    chat_env_on(provider, deps, crate::config::Channel::Stable).await
}

async fn chat_env_on(
    provider: Arc<dyn crate::llm::ChatProvider>,
    deps: ChatDeps,
    channel: crate::config::Channel,
) -> ChatEnv {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Arc::new(Paths::rooted(tmp.path()));
    paths.ensure_dirs().unwrap();
    let db = migrated_db();
    let frames = bus();
    let settings = crate::settings::Settings::with_file(
        tmp.path().join("settings.json"),
        db.clone(),
        Arc::clone(&frames),
    );
    let vault = crate::secrets::Vault::with_store(
        tmp.path().join("secrets.enc"),
        Arc::new(crate::secrets::testing::MemoryStore::new()),
    );
    let llm = crate::llm::Llm::with_provider(vault.clone(), channel, provider);
    let store = SessionsStore::new(Arc::clone(&paths));
    let chat = Chat::new(ChatConfig {
        sessions: store.clone(),
        memories: crate::memories::MemoriesStore::new(Arc::clone(&paths), db),
        embedding: crate::memories::Embedding::unwired(settings.clone(), vault),
        settings: settings.clone(),
        llm,
        frames: Arc::clone(&frames),
        deps,
        summaries: Arc::new(crate::memories::indexer::NoSummaries),
    });
    ChatEnv {
        _tmp: tmp,
        chat,
        store,
        frames,
        settings,
    }
}

impl ChatEnv {
    /// A connection for `client`, drained of the setup's own frames.
    fn conn(&self, client: &str) -> Connection {
        let mut conn = self.frames.register(client);
        let _ = drain(&mut conn);
        conn
    }

    /// A session owned by ALICE holding one user message.
    async fn asked(&self, text: &str) -> String {
        let session = new_session(&self.store, ALICE).await;
        self.store
            .insert_message_after(&session, user_message(text), None)
            .await
            .unwrap();
        session
    }

    async fn patch(&self, value: Value) {
        self.settings.patch(settings_of(value)).await.unwrap();
    }

    async fn local(&self) {
        self.patch(json!({ "llm.provider": "local" })).await;
    }

    /// Local, plus a fully configured secondary endpoint, so a routing test
    /// asserts the switch rather than the absence of configuration.
    async fn dual_model(&self, enabled: bool) {
        self.patch(json!({
            "llm.provider": "local",
            "dualModel.enabled": enabled,
            "dualModel.external.baseUrl": SECONDARY_URL,
            "dualModel.external.model": "fast-model",
            "dualModel.external.apiKey": "sk-secondary",
        }))
        .await;
    }
}

/// A [`ChatEnv`] over a provider that answers `text`, with no tool host.
async fn text_env(text: &str) -> ChatEnv {
    chat_env(
        Arc::new(crate::llm::testing::ScriptedProvider::text(text)),
        ChatDeps::unwired(),
    )
    .await
}

/// The same over a provider that never finishes, so a turn is genuinely still
/// running when the next frame arrives.
async fn wedged_env() -> ChatEnv {
    chat_env(
        Arc::new(
            crate::llm::testing::ScriptedProvider::text("slow")
                .with_gap(std::time::Duration::from_secs(3600)),
        ),
        ChatDeps::unwired(),
    )
    .await
}

/// A [`ChatEnv`] plus the provider it was built over, for asserting on the
/// endpoint each request was handed.
async fn recording_env(text: &str) -> (ChatEnv, Arc<crate::llm::testing::ScriptedProvider>) {
    let provider = Arc::new(crate::llm::testing::ScriptedProvider::text(text));
    let env = chat_env(
        Arc::clone(&provider) as Arc<dyn crate::llm::ChatProvider>,
        ChatDeps::unwired(),
    )
    .await;
    (env, provider)
}

fn start_frame(session_id: &str, stream_id: &str) -> crate::contract::inbound::ChatStartWs {
    serde_json::from_value(json!({
        "kind": "chat.start",
        "sessionId": session_id,
        "streamId": stream_id,
        "systemPrompt": "",
    }))
    .unwrap()
}

/// A base URL nothing else in this suite uses, so a request that reached it can
/// only have come from the secondary route.
const SECONDARY_URL: &str = "https://secondary.example.test/v1";

/// [`start_frame`] with the client PINNING a route, which an absent `route` has
/// to stay distinguishable from.
fn pinned_frame(
    session_id: &str,
    stream_id: &str,
    route: &str,
) -> crate::contract::inbound::ChatStartWs {
    serde_json::from_value(json!({
        "kind": "chat.start",
        "sessionId": session_id,
        "streamId": stream_id,
        "systemPrompt": "",
        "route": route,
    }))
    .unwrap()
}

/// Wait for the turn to register, bounded. An unbounded spin here is not a
/// slow test: a turn that exits early makes the condition unreachable and the
/// loop burns a core for the life of the suite, starving every other test.
async fn wait_active(chat: &Chat, session: &str) {
    for _ in 0..3_000 {
        if chat.active_session_ids().contains(session) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    panic!("the turn never registered an active stream on {session}");
}

/// Drain until `kind` shows up or the turn task has clearly finished. Bounded so
/// a wedged turn fails the test rather than hanging the suite.
async fn drain_until(conn: &mut Connection, kind: &str) -> Vec<Value> {
    let mut seen = Vec::new();
    for _ in 0..3_000 {
        seen.extend(drain(conn));
        if seen.iter().any(|f| f["kind"] == json!(kind)) {
            return seen;
        }
        // A real sleep, not `yield_now`. The turn runs on its own task and does
        // file I/O on the blocking pool, so a spin that never lets wall time
        // pass burns its whole budget in microseconds and reports "never sent"
        // for something that had not been given a chance to happen yet.
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    panic!("never saw {kind}; saw {:?}", kinds(&seen));
}

#[tokio::test]
async fn a_turn_persists_the_token_usage_it_broadcast() {
    // Live equals reload, for the context gauge: broadcasting usage without
    // persisting it leaves the gauge correct while the turn streams and empty
    // every time the session is reopened.
    let env = text_env("hello world").await;
    env.local().await;
    let session = env.asked("hi").await;
    let mut conn = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    let frames = drain_until(&mut conn, "chat.done").await;
    assert!(kinds(&frames).contains(&"chat.usage".to_string()));

    let usage = env
        .store
        .get_or_throw(ALICE, &session)
        .await
        .unwrap()
        .token_usage
        .expect("the usage the client was sent was never written to the session");
    assert_eq!((usage.prompt, usage.completion, usage.total), (10, 3, 13));
}

#[tokio::test]
async fn a_turn_streams_announces_and_persists_its_assistant_message() {
    let env = text_env("hello world").await;
    env.local().await;
    let session = env.asked("hi").await;
    let mut conn = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    let frames = drain_until(&mut conn, "chat.done").await;

    let seen = kinds(&frames);
    assert!(seen.contains(&"chat.message".to_string()), "{seen:?}");
    assert!(seen.contains(&"chat.delta".to_string()), "{seen:?}");
    let done = frames
        .iter()
        .find(|f| f["kind"] == json!("chat.done"))
        .unwrap();
    assert_eq!(done["reason"], json!("stop"));

    // Live equals reload: the assistant the client watched stream is in the
    // transcript with the same text.
    let persisted = env.store.list_messages(&session).await;
    let assistant = persisted
        .iter()
        .find(|m| m.role() == Some("assistant"))
        .expect("the assistant message was not persisted");
    assert_eq!(
        assistant.content().and_then(Value::as_str),
        Some("hello world"),
    );
}

#[tokio::test]
async fn a_second_start_on_the_same_session_is_refused_as_session_busy() {
    // The provider never finishes, so the first turn is genuinely still running
    // when the second frame arrives.
    let env = wedged_env().await;
    env.local().await;
    let session = new_session(&env.store, ALICE).await;
    let mut conn = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    wait_active(&env.chat, &session).await;
    env.chat.start(ALICE, start_frame(&session, "s2")).await;

    let frames = drain_until(&mut conn, "chat.error").await;
    let error = frames
        .iter()
        .find(|f| f["kind"] == json!("chat.error"))
        .unwrap();
    assert_eq!(error["code"], json!("session_busy"));
    assert_eq!(error["streamId"], json!("s2"));

    env.chat.shutdown().await;
}

#[tokio::test]
async fn a_different_client_may_run_a_turn_on_the_same_session() {
    // Reads like a bug and is not: every frame is scoped to its owner, so two
    // clients on one session cannot see each other's stream.
    let env = wedged_env().await;
    env.local().await;
    let session = new_session(&env.store, ALICE).await;
    let mut alice = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    wait_active(&env.chat, &session).await;
    // Same client, second stream: refused. The predicate is (client, session).
    env.chat.start(ALICE, start_frame(&session, "s2")).await;
    let frames = drain_until(&mut alice, "chat.error").await;
    assert_eq!(
        frames
            .iter()
            .find(|f| f["kind"] == json!("chat.error"))
            .unwrap()["code"],
        json!("session_busy"),
    );

    env.chat.shutdown().await;
}

#[tokio::test]
async fn an_unconfigured_external_provider_fails_fast_with_an_actionable_message() {
    let env = text_env("never reached").await;
    env.patch(json!({ "llm.provider": "external" })).await;
    let session = new_session(&env.store, ALICE).await;
    let mut conn = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    let frames = drain_until(&mut conn, "chat.error").await;

    let error = frames
        .iter()
        .find(|f| f["kind"] == json!("chat.error"))
        .unwrap();
    assert_eq!(error["code"], json!("provider_error"));
    assert!(
        error["message"].as_str().unwrap().contains("Base URL"),
        "an opaque message here is the whole thing this gate exists to avoid",
    );
}

#[tokio::test]
async fn a_model_that_only_calls_tools_stops_at_the_hop_limit() {
    // Every hop emits the same call, and nothing in a tool failure ends a turn,
    // so the loop bound is the only thing that stops this.
    let env = chat_env(
        Arc::new(crate::llm::testing::ScriptedProvider::from_items(vec![
            Ok(call_chunk(0, Some("call_a"), Some("nope"), Some("{}"))),
            Ok(finish("tool_calls")),
        ])),
        ChatDeps::unwired(),
    )
    .await;
    env.local().await;
    let session = env.asked("go").await;
    let mut conn = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    let frames = drain_until(&mut conn, "chat.done").await;

    assert_eq!(
        frames
            .iter()
            .find(|f| f["kind"] == json!("chat.done"))
            .unwrap()["reason"],
        json!("hop_limit"),
    );
    // Five hops, each persisting an assistant carrying the call plus the tool
    // message that answered it. Nothing is dropped on the way to the cap.
    let persisted = env.store.list_messages(&session).await;
    let tools = persisted
        .iter()
        .filter(|m| m.role() == Some("tool"))
        .count();
    assert_eq!(tools, 5, "got {:?}", persisted.len());
    // The unresolvable name is answered rather than dropped, so the model sees
    // why on its next hop.
    let failed = persisted
        .iter()
        .find(|m| m.role() == Some("tool"))
        .unwrap()
        .as_map()
        .clone();
    assert_eq!(failed["status"], json!("failed"));
    assert!(failed["error"].as_str().unwrap().contains("not available"));
}

#[tokio::test]
async fn an_interrupt_ends_the_turn_and_keeps_what_it_streamed() {
    let env = chat_env(
        Arc::new(
            crate::llm::testing::ScriptedProvider::from_items(vec![
                Ok(content("partial ")),
                Ok(content("answer")),
                Ok(finish("stop")),
            ])
            .with_gap(std::time::Duration::from_millis(20)),
        ),
        ChatDeps::unwired(),
    )
    .await;
    env.local().await;
    let session = env.asked("hi").await;
    let mut conn = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    // Wait for the first delta so there is something to keep, then interrupt.
    let _ = drain_until(&mut conn, "chat.delta").await;
    env.chat.interrupt("s1", ALICE);
    let frames = drain_until(&mut conn, "chat.done").await;

    assert_eq!(
        frames
            .iter()
            .find(|f| f["kind"] == json!("chat.done"))
            .unwrap()["reason"],
        json!("interrupted"),
    );
    let persisted = env.store.list_messages(&session).await;
    let assistant = persisted
        .iter()
        .find(|m| m.role() == Some("assistant"))
        .expect("the partial assistant was dropped rather than persisted");
    assert_eq!(assistant.as_map().get("interrupted"), Some(&json!(true)));
}

#[tokio::test]
async fn an_interrupt_from_another_client_is_ignored() {
    let env = wedged_env().await;
    env.local().await;
    let session = new_session(&env.store, ALICE).await;
    let mut conn = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    wait_active(&env.chat, &session).await;

    env.chat.interrupt("s1", BOB);
    // A real interval, not one yield: the mutation that drops the owner check
    // ends the turn, and the guard needs wall time to unwind. With a bare yield
    // this assertion held whether or not the check was there.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(env.chat.active_session_ids().contains(&session));

    // Paired: the owner's interrupt DOES land, so "still active" above is an
    // ignored control frame rather than an interrupt that never works.
    env.chat.interrupt("s1", ALICE);
    let _ = drain_until(&mut conn, "chat.done").await;
    env.chat.shutdown().await;
}

#[tokio::test]
async fn every_stream_deregisters_once_its_turn_ends() {
    // A leaked entry has no symptom other than the background queue's idle gate
    // never opening again, so memory summaries would silently stop running.
    let env = text_env("done").await;
    env.local().await;
    let session = new_session(&env.store, ALICE).await;
    let mut conn = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    let _ = drain_until(&mut conn, "chat.done").await;
    env.chat.shutdown().await;

    assert!(env.chat.active_session_ids().is_empty());
}

#[tokio::test]
async fn dual_model_routing_is_reachable_from_a_real_turn() {
    // Observed through the ENDPOINT the provider was handed, which is the only
    // externally visible consequence of the routing decision.
    let (env, provider) = recording_env("complex").await;
    env.dual_model(true).await;
    let session = env.asked("rewrite my compiler").await;
    let mut conn = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    let _ = drain_until(&mut conn, "chat.done").await;

    let seen = provider.requests();
    assert!(!seen.is_empty(), "the turn never reached the provider");
    // The classifier runs on the DEFAULT endpoint and answers "complex", so the
    // generation that follows must land on the secondary one. `any` rather than
    // an index: title generation also fires after a natural stop and resolves
    // its own endpoint, so the count is not the property under test.
    assert!(
        seen.iter().any(|r| r.endpoint.base_url == SECONDARY_URL),
        "no request reached the secondary endpoint; the classifier's answer was \
         not applied. endpoints: {:?}",
        seen.iter()
            .map(|r| &r.endpoint.base_url)
            .collect::<Vec<_>>(),
    );
}

/// The other direction, and it is not redundant: the test above passes against a
/// build that routes EVERYTHING to the secondary endpoint.
#[tokio::test]
async fn dual_model_off_keeps_every_request_on_the_default_endpoint() {
    let (env, provider) = recording_env("complex").await;
    env.dual_model(false).await;
    let session = env.asked("rewrite my compiler").await;
    let mut conn = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    let _ = drain_until(&mut conn, "chat.done").await;

    let seen = provider.requests();
    assert!(!seen.is_empty(), "the turn never reached the provider");
    assert!(
        !seen.iter().any(|r| r.endpoint.base_url == SECONDARY_URL),
        "auto-routing fired with dualModel.enabled off",
    );
}

/// A client that PINS a route must bypass the classifier entirely: `route` is
/// optional so absence means "decide for me", which only means anything if
/// presence still means "do not". A build ignoring it passes both tests above.
#[tokio::test]
async fn a_pinned_default_route_suppresses_auto_routing() {
    let (env, provider) = recording_env("complex").await;
    env.dual_model(true).await;
    let session = env.asked("rewrite my compiler").await;
    let mut conn = env.conn(ALICE);

    env.chat
        .start(ALICE, pinned_frame(&session, "s1", "default"))
        .await;
    let _ = drain_until(&mut conn, "chat.done").await;

    let seen = provider.requests();
    assert!(!seen.is_empty(), "the turn never reached the provider");
    assert!(
        !seen.iter().any(|r| r.endpoint.base_url == SECONDARY_URL),
        "a pinned route was overridden by the classifier",
    );
}

#[tokio::test]
async fn a_turn_keeps_the_channel_adjusted_local_port() {
    // `load_effective_resolved` carries the schema's `llm.port = "7701"`, which
    // is NOT channel-adjusted, and a present key beats the resolver's own
    // channel-aware fallback, so a `dev` core would talk to the stable channel's
    // llama-server. The turn therefore resolves the endpoint from the sparse
    // record instead.
    let provider = Arc::new(crate::llm::testing::ScriptedProvider::text("hi"));
    let env = chat_env_on(
        Arc::clone(&provider) as Arc<dyn crate::llm::ChatProvider>,
        ChatDeps::unwired(),
        crate::config::Channel::Dev,
    )
    .await;
    env.local().await;
    let session = env.asked("hi").await;
    let mut conn = env.conn(ALICE);

    env.chat.start(ALICE, start_frame(&session, "s1")).await;
    let _ = drain_until(&mut conn, "chat.done").await;

    // 7701 + Channel::Dev's offset of 20. A literal, not the constant: a test
    // written in terms of the offset it tests cannot see that offset change.
    // Every request, which is the hop plus the title generation that fires
    // after a natural stop: both resolve their endpoint the same way.
    let seen = provider.requests();
    assert!(!seen.is_empty(), "the turn never reached the provider");
    for request in &seen {
        assert_eq!(request.endpoint.base_url, "http://127.0.0.1:7721/v1");
    }
}
