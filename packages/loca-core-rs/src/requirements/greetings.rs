//! The greeting trigger.
//!
//! Ports `http/routes/greetings.ts`. The `greetings.*` settings are
//! client-on-client, so the CLIENT decides whether a given app start earns a
//! greeting and POSTs only when it should, carrying the title and instruction it
//! wants. The core does three things the client cannot do for itself: it mints
//! the session, it keeps a per-client dedup guard so a crash-looping client
//! cannot spawn a session per retry, and it holds the turn until the local model
//! has finished loading.
//!
//! The client navigates on the `session.created` frame, not on this response.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::chat::{run_automated_session, AutomatedSessionInput, Chat, LoadingProbe};
use crate::contract::api::{SessionCreatedFocus, SessionCreatedReason};
use crate::contract::inbound::GreetingRunBody;
use crate::settings::schema::schema;

/// The dedup window when the client supplies no cooldown.
///
/// A crash-looping client, or a buggy caller, must not mint a session per retry.
/// A suppressed run still answers 200 with `ran: false`, so an autostarted
/// launch can still tell that its window is open.
const MIN_INTERVAL_MS: f64 = 60_000.0;

/// The absolute floor for the dedup window, so a rogue or buggy client cannot
/// drive its own cooldown to zero and spam sessions.
const INTERVAL_FLOOR_MS: f64 = 1_000.0;

/// The fallback session title.
///
/// A literal, matching `greetings.ts:71`, rather than a schema read: a client
/// that sends no title is a client that is not using the `greetings.sessionTitle`
/// setting at all. `the_fallback_title_matches_the_settings_default` pins the
/// two together so they cannot drift apart unnoticed.
const DEFAULT_SESSION_TITLE: &str = "Greeting {datetime}";

/// How long to wait for the local model, and how often to look.
///
/// A field rather than two constants so a test can drive the defer path in
/// milliseconds. The production values are the `Default`, so nothing but a test
/// can change them and forgetting to set them cannot silently shorten the wait.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// How long to hold the greeting before starting it anyway. Starting it
    /// surfaces the error IN the session, which still reveals the window; never
    /// starting it leaves the user with nothing and no explanation.
    pub ready_timeout: Duration,
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ready_timeout: Duration::from_secs(180),
            poll: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RunResponse {
    pub ran: bool,
    /// Omitted rather than null when the greeting ran, matching the oracle's
    /// two distinct object literals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

struct Inner {
    chat: Chat,
    probe: Arc<dyn LoadingProbe>,
    timing: Timing,
    /// Last greeting per client id, pruned past the dedup window so it cannot
    /// grow without bound across distinct clients.
    last: Mutex<HashMap<String, i64>>,
}

/// The greeting service. Cheap to clone; every clone shares one dedup map,
/// which is the whole point: two would each allow their own greeting per window.
#[derive(Clone)]
pub struct Greetings {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Greetings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Greetings").finish_non_exhaustive()
    }
}

impl Greetings {
    pub fn new(chat: Chat, probe: Arc<dyn LoadingProbe>) -> Self {
        Self::with_timing(chat, probe, Timing::default())
    }

    pub fn with_timing(chat: Chat, probe: Arc<dyn LoadingProbe>, timing: Timing) -> Self {
        Self {
            inner: Arc::new(Inner {
                chat,
                probe,
                timing,
                last: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Mint a greeting session for `client_id`, unless one already ran inside
    /// this client's window.
    ///
    /// Returns as soon as the deferred runner is spawned. The turn itself waits
    /// for the model.
    /// The body is `inbound::GreetingRunBody`, generated from
    /// `validation/greetings.ts`. `.strict()` survives as `deny_unknown_fields`
    /// (D-031 settled that the schema decides strictness, not the direction of
    /// travel), and `cooldownMs` is `f64` because the Zod field is a bare
    /// `z.number()`: a client may send a fractional cooldown and the oracle
    /// accepts it.
    pub fn run(&self, client_id: &str, body: GreetingRunBody) -> RunResponse {
        // Both bounds matter and for different reasons: the default is the
        // window when the client sends none, and the floor is what stops a
        // client asking for a zero-length window.
        let interval = body
            .cooldown_ms
            .unwrap_or(MIN_INTERVAL_MS)
            .max(INTERVAL_FLOOR_MS) as i64;
        let now = now_ms();
        {
            let mut last = lock(&self.inner.last);
            if last.get(client_id).is_some_and(|t| now - t < interval) {
                return RunResponse {
                    ran: false,
                    reason: Some("recent"),
                };
            }
            // Recorded BEFORE the turn is spawned, so a client that crashes
            // mid-greeting is still deduped on its next attempt.
            last.insert(client_id.to_string(), now);
            let cutoff = now - interval;
            last.retain(|_, t| *t >= cutoff);
        }

        // Trimmed-empty falls back too, not just absent: `"   "` is a client
        // sending an unset field, and greeting with whitespace is not a
        // greeting.
        let instruction = non_blank(body.instruction).unwrap_or_else(default_instruction);
        let title =
            non_blank(body.session_title).unwrap_or_else(|| DEFAULT_SESSION_TITLE.to_string());

        let inner = Arc::clone(&self.inner);
        let owner = client_id.to_string();
        tokio::spawn(async move { run_when_ready(inner, owner, title, instruction).await });
        RunResponse {
            ran: true,
            reason: None,
        }
    }
}

/// Hold the greeting until the local model is up, then open the session.
///
/// The connected edge that triggers this POST is exactly when the model begins
/// loading on boot, so an immediate turn would race the load and error. This is
/// the same defer the scheduler tick performs, through the same predicate.
async fn run_when_ready(inner: Arc<Inner>, owner: String, title: String, instruction: String) {
    let deadline = tokio::time::Instant::now() + inner.timing.ready_timeout;
    while inner.probe.still_loading().await {
        if tokio::time::Instant::now() > deadline {
            tracing::warn!(
                target: "loca::requirements",
                "local model still loading after the wait budget; starting the greeting anyway",
            );
            break;
        }
        tokio::time::sleep(inner.timing.poll).await;
    }
    if let Err(err) = run_automated_session(
        &inner.chat,
        AutomatedSessionInput {
            owner_client_id: owner,
            title,
            instruction,
            reason: SessionCreatedReason::Greeting,
            scheduled_prompt_id: None,
            // Navigate silently, reveal the window when the turn finishes: a
            // greeting must not steal focus the moment the app starts.
            focus: SessionCreatedFocus::ShowWhenDone,
        },
    )
    .await
    {
        tracing::error!(target: "loca::requirements", "greeting failed to start: {err}");
    }
}

/// The greeting prompt a client that sent none gets.
///
/// Read from the settings schema rather than copied, so there is one string.
/// The schema's `greetings.instruction` default IS `DEFAULT_GREETING_INSTRUCTION`
/// (`settings/groups/greetings.ts:74`), and `the_default_instruction_comes_from_
/// the_schema` fails loudly if that field is ever renamed away, rather than
/// letting the core greet with an empty prompt.
fn default_instruction() -> String {
    schema()
        .field("greetings.instruction")
        .and_then(|f| f.default_value.as_ref())
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn non_blank(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::chat::TITLE_DATETIME_PLACEHOLDER;

    /// The route's fallback and the setting's default are two copies of one
    /// string, and the oracle has the same two copies. This is what makes the
    /// duplication safe rather than a drift waiting to happen.
    #[test]
    fn the_fallback_title_matches_the_settings_default() {
        let default = schema()
            .field("greetings.sessionTitle")
            .and_then(|f| f.default_value.as_ref())
            .and_then(|v| v.as_str());
        assert_eq!(default, Some(DEFAULT_SESSION_TITLE));
        // And it actually carries the placeholder, so a greeting session is not
        // titled the literal word "Greeting" over and over.
        assert!(DEFAULT_SESSION_TITLE.contains(TITLE_DATETIME_PLACEHOLDER));
    }

    /// A schema read that silently answered "" would leave every greeting with
    /// an empty prompt, which the model would answer with something unrelated
    /// rather than with an error anyone could trace.
    #[test]
    fn the_default_instruction_comes_from_the_schema() {
        let instruction = default_instruction();
        assert!(instruction.len() > 40, "{instruction:?}");
        assert!(instruction.starts_with("Greet me"), "{instruction:?}");
    }

    #[test]
    fn a_blank_or_absent_field_falls_back_and_a_real_one_is_trimmed() {
        assert_eq!(non_blank(None), None);
        assert_eq!(non_blank(Some("   ".to_string())), None);
        assert_eq!(non_blank(Some("\n\t".to_string())), None);
        assert_eq!(
            non_blank(Some("  hi  ".to_string())),
            Some("hi".to_string())
        );
    }
}
