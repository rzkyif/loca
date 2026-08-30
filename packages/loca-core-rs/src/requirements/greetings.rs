//! The greeting trigger. The client decides whether an app start earns a
//! greeting; the core mints the session, dedups per client so a crash-looping
//! client cannot spawn a session per retry, and holds the turn until the local
//! model has loaded. The client navigates on `session.created`, not on this response.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::chat::{run_automated_session, AutomatedSessionInput, Chat, LoadingProbe};
use crate::contract::api::{SessionCreatedFocus, SessionCreatedReason};
use crate::contract::inbound::GreetingRunBody;
use crate::settings::schema::schema;

/// The dedup window when the client supplies no cooldown. A suppressed run
/// still answers 200 with `ran: false`.
const MIN_INTERVAL_MS: f64 = 60_000.0;

/// The floor for the dedup window, so a client cannot drive it to zero.
const INTERVAL_FLOOR_MS: f64 = 1_000.0;

/// The fallback session title; a test pins it to the settings default.
const DEFAULT_SESSION_TITLE: &str = "Greeting {datetime}";

/// How long to wait for the local model, and how often to look. Injectable so
/// a test can drive the defer path in milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// How long to hold the greeting before starting it anyway (the error then
    /// surfaces in the session rather than nowhere).
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
    /// Omitted rather than null when the greeting ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

struct Inner {
    chat: Chat,
    probe: Arc<dyn LoadingProbe>,
    timing: Timing,
    /// Last greeting per client id, pruned past the dedup window.
    last: Mutex<HashMap<String, i64>>,
}

/// The greeting service. Cheap to clone; every clone shares one dedup map.
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
    /// this client's window. Returns as soon as the deferred runner is spawned.
    pub fn run(&self, client_id: &str, body: GreetingRunBody) -> RunResponse {
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
            // Recorded before the spawn, so a mid-greeting crash is still deduped.
            last.insert(client_id.to_string(), now);
            let cutoff = now - interval;
            last.retain(|_, t| *t >= cutoff);
        }

        // Blank falls back too, not just absent.
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

/// Hold the greeting until the local model is up, then open the session: the
/// POST lands exactly when the boot-time model load begins.
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
            // A greeting must not steal focus the moment the app starts.
            focus: SessionCreatedFocus::ShowWhenDone,
        },
    )
    .await
    {
        tracing::error!(target: "loca::requirements", "greeting failed to start: {err}");
    }
}

/// The greeting prompt a client that sent none gets, read from the settings
/// schema so there is one string.
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

    #[test]
    fn the_fallback_title_matches_the_settings_default() {
        let default = schema()
            .field("greetings.sessionTitle")
            .and_then(|f| f.default_value.as_ref())
            .and_then(|v| v.as_str());
        assert_eq!(default, Some(DEFAULT_SESSION_TITLE));
        assert!(DEFAULT_SESSION_TITLE.contains(TITLE_DATETIME_PLACEHOLDER));
    }

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
