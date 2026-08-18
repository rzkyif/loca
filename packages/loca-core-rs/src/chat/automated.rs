//! Core-initiated ("automated") sessions, and the readiness predicate that
//! decides when one may start.
//!
//! Work package **D8**, living in D4's module by ruling (D-090, D-094). Both
//! things here have exactly two callers, greetings (D8) and scheduled prompts
//! (D10), and both were about to be ported twice. They are here rather than
//! under `requirements/` because the concept is a core-initiated chat turn: a
//! `scheduled -> requirements` module edge would state something false, and
//! module edges are read by people deciding what may call what.
//!
//! # What an automated session is
//!
//! A fresh session for the owner client, with the instruction persisted as an
//! `automated: true` user message, announced over WS as `session.created`, and
//! then a normal chat turn started on it.
//!
//! The turn deliberately runs the standard pipeline. No `systemPrompt` on the
//! frame means the turn falls back to the `prompts.defaultSystemPrompt` core
//! setting, and [`DEFAULT_TOOLS_HINT`] is appended only if the turn actually
//! exposes tools. Everything persists even when the owner client is offline:
//! `broadcast_to_client` drops frames nobody is listening for, and the client
//! catches up from the session list on its next connect.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::contract::api::{
    ServerToClientFrame, Session, SessionCreatedFocus, SessionCreatedReason, SidecarKind,
    SidecarStatus,
};
use crate::error::{internal, Result};
use crate::sessions::{CreateSessionInput, StoredMessage};
use crate::settings::access::str_setting;
use crate::settings::Settings;
use crate::sidecars::Manager;

use super::Chat;

/// Appended to an automated turn iff that turn exposes tools.
///
/// A copy of `@loca/shared`'s `DEFAULT_TOOLS_HINT` (`domain/prompts.ts:129`),
/// because the contract bridge emits types and not constants. Its one other
/// reader is the `[toolsAvailable:...]` block of the default system prompt, on
/// the TypeScript side only.
pub const DEFAULT_TOOLS_HINT: &str = "Tools are available for this turn. Use one only when the user's request actually needs it; for greetings or small talk, just reply normally without calling any tool.";

/// The placeholder a user may put in an automated session's title.
///
/// `domain/prompts.ts:151`. Persisted in the `greetings.sessionTitle` and
/// scheduled-prompt settings a user has already written, so the spelling is
/// fixed.
pub const TITLE_DATETIME_PLACEHOLDER: &str = "{datetime}";

/// What opening an automated session needs. Ports `AutomatedSessionInput`
/// (`services/automated-session.ts:23-33`).
#[derive(Clone, Debug)]
pub struct AutomatedSessionInput {
    pub owner_client_id: String,
    pub title: String,
    /// The automated user prompt that opens the session.
    pub instruction: String,
    pub reason: SessionCreatedReason,
    /// Set by scheduled prompts, absent for a greeting. It rides the
    /// `session.created` frame so the client can tie the new session back to the
    /// schedule row that opened it.
    pub scheduled_prompt_id: Option<String>,
    /// How the owner client should surface the new session.
    pub focus: SessionCreatedFocus,
}

/// Open a session, persist the instruction, announce it, and start the turn.
///
/// Returns as soon as the turn is spawned; the turn itself is fire-and-forget,
/// exactly as `automated-session.ts:60-69` is.
pub async fn run_automated_session(chat: &Chat, input: AutomatedSessionInput) -> Result<Session> {
    let now = now_ms();
    // `{datetime}` is stamped with the CREATION time, so a title of
    // "Greeting {datetime}" reads "Greeting 2026-06-15 14:30" and matches the
    // default-title form the client shows before title generation runs.
    let session = chat
        .inner
        .sessions
        .create(CreateSessionInput {
            owner_client_id: input.owner_client_id.clone(),
            title: Some(apply_title_datetime(&input.title, now)),
            temporary: false,
        })
        .await?;

    // The store assigns `id`, `ord` and `createdAtMs`, so they are deliberately
    // absent here rather than minted and immediately overwritten.
    let mut message = Map::new();
    message.insert("role".to_string(), Value::String("user".to_string()));
    message.insert(
        "content".to_string(),
        Value::String(input.instruction.clone()),
    );
    message.insert("automated".to_string(), Value::Bool(true));
    chat.inner
        .sessions
        .append_message(&session.id, StoredMessage::new(message))
        .await?;

    chat.inner.frames.broadcast_to_client(
        &input.owner_client_id,
        &ServerToClientFrame::SessionCreated {
            focus: input.focus,
            reason: input.reason,
            scheduled_prompt_id: input.scheduled_prompt_id.clone(),
            session: session.clone(),
        },
    );
    tracing::info!(
        target: "loca::chat",
        "automated session {} ({}): {:?}",
        session.id, input.reason, input.title,
    );

    chat.start(&input.owner_client_id, automated_start_frame(&session.id)?)
        .await;
    Ok(session)
}

/// The `chat.start` frame an automated turn runs on.
///
/// `route: default` and no `systemPrompt`: the turn resolves its own system
/// prompt from settings, which is what makes an automated turn identical to a
/// user-typed one from the model's point of view.
fn automated_start_frame(session_id: &str) -> Result<crate::contract::inbound::ChatStartWs> {
    use crate::contract::inbound::{ChatStartWs, ChatStartWsKind, ChatStartWsRoute};
    let stream_id = ulid::Ulid::new().to_string();
    Ok(ChatStartWs {
        anchor_message_id: None,
        kind: ChatStartWsKind::ChatStart,
        route: Some(ChatStartWsRoute::Default),
        // Both are `minLength: 1` newtypes. A freshly minted ulid and a session
        // id the store just returned both satisfy that, so a failure here is a
        // broken invariant rather than bad input, and it is reported as one.
        session_id: session_id
            .parse()
            .map_err(|_| internal("an automated session's own id is not a valid session id"))?,
        stream_id: stream_id
            .parse()
            .map_err(|_| internal("a freshly minted stream id is not a valid stream id"))?,
        system_prompt: None,
        tools_hint: Some(DEFAULT_TOOLS_HINT.to_string()),
        extra: Map::new(),
    })
}

/// Replace every `{datetime}` with the LOCAL `YYYY-MM-DD HH:MM` for `ms`.
///
/// Local, not UTC. `domain/prompts.ts:157-163` builds this from `Date`'s local
/// getters, and the string is a session title a user reads next to their own
/// clock. `Utc` here would compile, pass every test written in London, and be
/// wrong everywhere else.
pub fn apply_title_datetime(title: &str, ms: i64) -> String {
    if !title.contains(TITLE_DATETIME_PLACEHOLDER) {
        return title.to_string();
    }
    let stamp = chrono::DateTime::from_timestamp_millis(ms)
        .map(|utc| {
            chrono::DateTime::<chrono::Local>::from(utc)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default();
    title.replace(TITLE_DATETIME_PLACEHOLDER, &stamp)
}

// --- readiness ---------------------------------------------------------------

/// Whether an automated turn may start yet.
///
/// The seam, so a caller's call sites do not move as the implementation lands
/// and so a test can drive the defer path without a model. D10 declared this
/// shape (`docs/rust-port/packages/D10/plan.md`) and it lives here with its
/// implementation, per D-094.
#[async_trait]
pub trait LoadingProbe: Send + Sync {
    async fn still_loading(&self) -> bool;
}

/// The production probe: true only while the LOCAL language model is mid-load.
///
/// Ports `llmStillLoading` (`services/prompt-scheduler.ts:426-431`). An
/// automated turn fired during the load would race it and error, so both
/// callers hold until this goes false.
///
/// **A core with no in-process engine answers false**, because nothing publishes
/// [`SidecarKind::Llama`] and an unpublished kind reads as `Disabled`
/// (`sidecars/registry.rs:109`). That is the honest answer rather than a stub:
/// there is no load to wait for, so the turn starts immediately and fails or
/// succeeds on its own merits. It starts deferring the moment N2 publishes.
pub struct LlmLoading {
    settings: Settings,
    sidecars: Arc<Manager>,
}

impl LlmLoading {
    pub fn new(settings: Settings, sidecars: Arc<Manager>) -> Self {
        Self { settings, sidecars }
    }
}

#[async_trait]
impl LoadingProbe for LlmLoading {
    async fn still_loading(&self) -> bool {
        // `load`, not `load_resolved`: the oracle reads the raw store
        // (`loadCoreSettings`) and treats a provider that is absent as local,
        // which the `!= "external"` test below already does.
        let settings = match self.settings.load().await {
            Ok(settings) => settings,
            // A settings read that fails must not hold an automated turn
            // forever. Reporting "not loading" starts the turn, which then
            // succeeds or reports its own error in the session where the user
            // can see it.
            Err(err) => {
                tracing::warn!(
                    target: "loca::chat",
                    "could not read settings for the automated-turn readiness check: {err}",
                );
                return false;
            }
        };
        // Absent, or set to something that is not a string, defaults to local
        // and falls through to the sidecar check. That is the oracle's
        // `typeof provider === "string" && provider !== "local"` exactly: only a
        // STRING naming a non-local provider short-circuits.
        if str_setting(&settings, "llm.provider", "local") != "local" {
            return false;
        }
        self.sidecars.status(SidecarKind::Llama).status == SidecarStatus::Loading
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_title_without_the_placeholder_is_returned_unchanged() {
        assert_eq!(
            apply_title_datetime("Morning briefing", 0),
            "Morning briefing"
        );
    }

    /// Both halves, because a stamper that returned the empty string would pass
    /// a "does not contain {datetime}" assertion on its own.
    #[test]
    fn every_placeholder_is_replaced_with_a_local_minute_stamp() {
        let out = apply_title_datetime("{datetime} / {datetime}", 1_781_000_000_000);
        assert!(!out.contains(TITLE_DATETIME_PLACEHOLDER), "{out}");
        let parts: Vec<&str> = out.split(" / ").collect();
        assert_eq!(parts.len(), 2, "{out}");
        assert_eq!(parts[0], parts[1], "one instant, two identical stamps");
        // YYYY-MM-DD HH:MM, and nothing looser: a format string that emitted
        // seconds or an RFC3339 timestamp would still be "not the placeholder".
        let stamp = parts[0];
        assert_eq!(stamp.len(), 16, "{stamp}");
        assert_eq!(stamp.as_bytes()[4], b'-');
        assert_eq!(stamp.as_bytes()[7], b'-');
        assert_eq!(stamp.as_bytes()[10], b' ');
        assert_eq!(stamp.as_bytes()[13], b':');
        assert!(
            stamp.chars().filter(char::is_ascii_digit).count() == 12,
            "{stamp}"
        );
    }

    /// The stamp follows the LOCAL clock, which is the whole reason this is not
    /// `Utc::now`. Asserted by comparing against `chrono::Local` for the same
    /// instant rather than against a hardcoded string, which would only be right
    /// in one zone.
    ///
    /// **Known limit, recorded rather than hidden**: on a host whose zone IS
    /// UTC (most CI) `Local` and `Utc` agree, so this cannot discriminate there
    /// and a green run proves only the format. It was watched catching a staged
    /// `Utc` on a non-UTC developer host. `chrono::Local` reads a process-global
    /// zone that a parallel test binary must not mutate, so forcing a zone here
    /// would be worse than the gap.
    #[test]
    fn the_stamp_is_local_time_rather_than_utc() {
        let ms = 1_781_000_000_000;
        let expected = chrono::DateTime::<chrono::Local>::from(
            chrono::DateTime::from_timestamp_millis(ms).unwrap(),
        )
        .format("%Y-%m-%d %H:%M")
        .to_string();
        assert_eq!(apply_title_datetime("{datetime}", ms), expected);
    }

    // --- the readiness predicate ---------------------------------------------
    //
    // D-094's shared backend. Both callers hold an automated turn on this
    // answer, so every arm of it is exercised: a wrong `false` fires a turn into
    // a loading model and errors, and a wrong `true` holds a greeting for three
    // minutes on a core with no local model at all.

    use crate::contract::api::SidecarSnapshot;
    use crate::sidecars::Manager;

    fn loading_probe(manager: &Arc<Manager>) -> (tempfile::TempDir, LlmLoading) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call_blocking(|conn| crate::db::migrate(conn)).unwrap();
        let settings = Settings::with_file(
            tmp.path().join("settings.json"),
            db,
            Arc::new(crate::transport::FrameBus::new()),
        );
        (tmp, LlmLoading::new(settings, Arc::clone(manager)))
    }

    fn publish(manager: &Manager, status: crate::contract::api::SidecarStatus) {
        manager.publish(SidecarSnapshot {
            cpu_pct: None,
            kind: SidecarKind::Llama,
            message: None,
            pid: None,
            progress: None,
            rss_mb: None,
            status,
        });
    }

    /// The three states of the local engine, through one probe, so "true" is
    /// shown to be about Loading specifically rather than about anything being
    /// published.
    #[tokio::test]
    async fn a_local_model_holds_the_turn_only_while_it_is_loading() {
        let manager = Arc::new(Manager::new());
        let (_tmp, probe) = loading_probe(&manager);

        // Nothing published: no engine on this core, so there is no load to wait
        // for. The correct answer is false, not a three-minute hold.
        assert!(!probe.still_loading().await);

        publish(&manager, SidecarStatus::Loading);
        assert!(probe.still_loading().await);

        publish(&manager, SidecarStatus::Running);
        assert!(!probe.still_loading().await);
    }

    /// An external provider is never "loading": there is no local model, so the
    /// sidecar's state is irrelevant and must not hold the turn.
    #[tokio::test]
    async fn an_external_provider_never_holds_the_turn() {
        let manager = Arc::new(Manager::new());
        let (_tmp, probe) = loading_probe(&manager);
        publish(&manager, SidecarStatus::Loading);
        // The paired positive first, so the assertion below is about the
        // provider rather than about the probe never answering true.
        assert!(probe.still_loading().await);

        let mut record = serde_json::Map::new();
        record.insert("llm.provider".to_string(), Value::String("external".into()));
        probe.settings.patch(record).await.unwrap();
        assert!(!probe.still_loading().await);
    }

    /// An absent or non-string provider means LOCAL, matching the oracle's
    /// `typeof provider === "string" && provider !== "local"`: only a string
    /// naming another provider short-circuits.
    #[tokio::test]
    async fn a_missing_or_malformed_provider_is_treated_as_local() {
        let manager = Arc::new(Manager::new());
        let (_tmp, probe) = loading_probe(&manager);
        publish(&manager, SidecarStatus::Loading);

        // Absent.
        assert!(probe.still_loading().await);

        // Present but not a string. The settings validator would refuse this
        // over the API; a hand-edited file would not be refused at all.
        let mut record = serde_json::Map::new();
        record.insert("llm.provider".to_string(), Value::Bool(true));
        let _ = probe.settings.patch(record).await;
        assert!(probe.still_loading().await);
    }

    /// A settings store that cannot be read must not hold an automated turn
    /// forever. Answering "not loading" starts the turn, which then succeeds or
    /// reports its own error in the session, where the user can see it; the
    /// alternative is a greeting that silently never arrives.
    ///
    /// The unreadable store is a DIRECTORY where the file should be, which is
    /// the one failure a test can stage without a filesystem fault injector.
    #[tokio::test]
    async fn an_unreadable_settings_store_does_not_hold_the_turn() {
        let manager = Arc::new(Manager::new());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::create_dir(&path).unwrap();
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call_blocking(|conn| crate::db::migrate(conn)).unwrap();
        let probe = LlmLoading::new(
            Settings::with_file(path, db, Arc::new(crate::transport::FrameBus::new())),
            Arc::clone(&manager),
        );
        // Published as Loading, so a probe that ignored the failed read and
        // reached the sidecar would answer true. Only the early return makes
        // this false.
        publish(&manager, SidecarStatus::Loading);

        assert!(!probe.still_loading().await);
    }
}
