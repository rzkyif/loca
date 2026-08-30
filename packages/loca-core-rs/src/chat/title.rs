//! Session title generation. After a session's first turn stops naturally the
//! model is asked for a short title, which is PATCHed onto the session. The
//! automatic path is fire and forget; the regenerate button is the same
//! function with `force`.
//!
//! The spinner is always paired: once a run commits, `session.updated
//! { title_generating: true }` goes out and the matching `false` is sent on
//! EVERY exit, including the two returns after it. The three guards that run
//! BEFORE it emit neither, so a no-op run does not flash a spinner.
//!
//! Not `single_shot`: this calls the provider directly so title generation
//! never queues behind a chat turn, and it carries two bounds the collected
//! -string helper does not, a 30-second deadline and a cap on the RAW
//! accumulated text so [`sanitize`]'s regexes cannot be handed an unbounded
//! string by a provider ignoring `max_tokens`.

use std::sync::Arc;

use futures_util::stream::StreamExt as _;
use regex::Regex;
use serde_json::Value;
use tokio::time::Duration;

use crate::contract::api::{
    ServerToClientFrame, ServerToClientFrameOp, ServerToClientFramePayload,
};
use crate::llm::{
    resolve_context_size, thinking_budget, ChatMessage, Llm, LlmRequest, LlmRoute, Overrides,
};
use crate::sessions::doc::StoredMessage;
use crate::sessions::SessionsStore;
use crate::settings::access::str_setting;
use crate::settings::{schema, Settings, SettingsRecord};
use crate::transport::FrameBus;

/// Hard cap on the stored title. The model is asked for about five words;
/// an adversarial or runaway one ignores that.
const MAX_TITLE_CHARS: usize = 80;

/// Cap on the raw streamed output accumulated before sanitizing. Generous
/// enough for any legitimate title plus a stray `<think>` block.
const MAX_TITLE_RAW_CHARS: usize = 4096;

/// This call runs outside the scheduler and its watchdog, so it carries its own
/// deadline: a wedged endpoint must not keep a fire-and-forget task alive for
/// the life of the process.
const TITLE_GEN_TIMEOUT_MS: u64 = 30_000;

/// Output allowance reserved for the title itself, on top of the thinking
/// budget, when deciding how much transcript fits the context window.
const TITLE_OUTPUT_TOKENS: u32 = 32;

/// Slack left under the context window so a slightly-off character estimate
/// cannot tip the request into an overflow error.
const TITLE_SAFETY_MARGIN_TOKENS: i64 = 64;

/// Cheap to clone: a fire-and-forget title task takes its own clone rather than
/// borrowing the orchestrator.
#[derive(Clone)]
pub struct TitleGen {
    sessions: SessionsStore,
    settings: Settings,
    llm: Llm,
    frames: Arc<FrameBus>,
}

impl TitleGen {
    pub fn new(
        sessions: SessionsStore,
        settings: Settings,
        llm: Llm,
        frames: Arc<FrameBus>,
    ) -> Self {
        Self {
            sessions,
            settings,
            llm,
            frames,
        }
    }

    /// The automatic path, run after a session's first turn stops. A no-op
    /// unless the session has no title yet, so calling it on every stop costs
    /// two reads.
    pub async fn maybe_generate(&self, session_id: &str, owner_client_id: &str) {
        self.generate(session_id, owner_client_id, false).await;
    }

    /// The manual path. Replaces any existing title.
    pub async fn regenerate(&self, session_id: &str, owner_client_id: &str) {
        self.generate(session_id, owner_client_id, true).await;
    }

    async fn generate(&self, session_id: &str, owner_client_id: &str, force: bool) {
        let Ok(session) = self
            .sessions
            .get_or_throw(owner_client_id, session_id)
            .await
        else {
            return;
        };
        // A temporary session shows a fixed label in the bar, so a title would
        // be wasted work and a wasted model call.
        if session.temporary == Some(true) {
            return;
        }
        if !force && !session.title.trim().is_empty() {
            return;
        }

        // Committed to a run. Everything below this line exits through
        // `signal(false)`.
        self.signal(owner_client_id, session_id, true);
        let outcome = self.run(session_id, owner_client_id).await;
        match outcome {
            Ok(Some(title)) => {
                if let Err(err) = self
                    .sessions
                    .patch_title(owner_client_id, session_id, &title)
                    .await
                {
                    tracing::warn!(
                        target: "loca::title-gen",
                        "title gen failed for {session_id}: {err}",
                    );
                } else {
                    self.frames.broadcast_to_client(
                        owner_client_id,
                        &ServerToClientFrame::SessionUpdated {
                            op: ServerToClientFrameOp::TitleChanged,
                            payload: Some(ServerToClientFramePayload {
                                title: Some(title),
                                ..ServerToClientFramePayload::default()
                            }),
                            session_id: session_id.to_string(),
                        },
                    );
                }
            }
            Ok(None) => {}
            Err(err) => tracing::warn!(
                target: "loca::title-gen",
                "title gen failed for {session_id}: {err}",
            ),
        }
        self.signal(owner_client_id, session_id, false);
    }

    /// The generating half. `Ok(None)` is "nothing to title", which is a normal
    /// outcome rather than a failure.
    async fn run(
        &self,
        session_id: &str,
        owner_client_id: &str,
    ) -> crate::error::Result<Option<String>> {
        let messages = self.sessions.list_messages(session_id).await;
        // Both halves of a turn are required: a session with a user message and
        // no reply has nothing to name yet.
        let has_user = messages.iter().any(|m| m.role() == Some("user"));
        let has_assistant = messages.iter().any(|m| m.role() == Some("assistant"));
        if !has_user || !has_assistant {
            return Ok(None);
        }

        // The owner's title prompt and thinking budget, resolved so the
        // schema's default reaches the budget. The endpoint and context size
        // come from the SPARSE record; see `turn.rs`.
        let settings = self
            .settings
            .load_effective_resolved(owner_client_id)
            .await?;
        let sparse = self.settings.load_effective(owner_client_id).await?;
        let template = template(&settings);
        let endpoint = self
            .llm
            .resolve_endpoint(&sparse, LlmRoute::Default)
            .await?;
        // Defaults to 128, and 0 turns thinking off for this call. `max_tokens`
        // carries the budget on top of the title's own allowance so thinking
        // cannot crowd the answer out.
        let budget = thinking_budget(&settings, "prompts.titleGenerationThinkingBudget");
        let context_size = resolve_context_size(&sparse, LlmRoute::Default);

        let context = select_title_context(
            &messages,
            &template,
            i64::from(context_size),
            i64::from(TITLE_OUTPUT_TOKENS + budget),
        );
        let system_prompt = fill_template(&template, &context);

        let request = LlmRequest::new(
            endpoint,
            vec![
                ChatMessage::system(system_prompt),
                ChatMessage::user("Title:"),
            ],
        )
        .with_overrides(Overrides {
            temperature: Some(0.3),
            max_tokens: Some(TITLE_OUTPUT_TOKENS + budget),
            reasoning_budget: Some(budget),
            ..Overrides::default()
        });

        let raw = tokio::time::timeout(
            Duration::from_millis(TITLE_GEN_TIMEOUT_MS),
            self.accumulate(request),
        )
        .await;
        let raw = match raw {
            Ok(raw) => raw?,
            // The deadline is not an error worth surfacing: the session simply
            // stays untitled and the next stop tries again.
            Err(_) => {
                tracing::warn!(
                    target: "loca::title-gen",
                    "title gen for {session_id} timed out after {TITLE_GEN_TIMEOUT_MS}ms",
                );
                return Ok(None);
            }
        };
        let title = sanitize(&raw);
        Ok(if title.is_empty() { None } else { Some(title) })
    }

    /// Collect content deltas up to the raw cap. Returning early drops the
    /// stream, which tears the upstream connection down.
    async fn accumulate(&self, request: LlmRequest) -> crate::error::Result<String> {
        let mut stream = self.llm.stream_unqueued(request);
        let mut raw = String::new();
        while let Some(item) = stream.next().await {
            if let Some(delta) = item?.content_delta {
                raw.push_str(&delta);
            }
            if raw.chars().count() > MAX_TITLE_RAW_CHARS {
                break;
            }
        }
        Ok(raw)
    }

    fn signal(&self, owner_client_id: &str, session_id: &str, generating: bool) {
        self.frames.broadcast_to_client(
            owner_client_id,
            &ServerToClientFrame::SessionUpdated {
                op: ServerToClientFrameOp::TitleGenerating,
                payload: Some(ServerToClientFramePayload {
                    generating: Some(generating),
                    ..ServerToClientFramePayload::default()
                }),
                session_id: session_id.to_string(),
            },
        );
    }
}

/// The template, falling back to the schema default when the stored value is
/// blank: a user who clears the field gets the default prompt back rather than
/// an empty system message, which the shared reader does not cover.
fn template(settings: &SettingsRecord) -> String {
    let stored = str_setting(settings, "prompts.titleGenerationPrompt", "");
    if !stored.is_empty() {
        return stored;
    }
    schema()
        .defaults()
        .get("prompts.titleGenerationPrompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The opening message, and the newest turns that fit the context window.
pub struct TitleContext {
    pub first_message: String,
    pub recent_messages: String,
}

/// Budget the transcript against the context window. The opening turn is always
/// included AND excluded from the newest-first walk, because the template shows
/// it separately and counting it twice wastes the budget.
pub fn select_title_context(
    messages: &[StoredMessage],
    template: &str,
    context_size: i64,
    output_allowance: i64,
) -> TitleContext {
    // Only conversational turns carry topic signal: tool calls, reasoning
    // traces and empty messages carry none.
    let turns: Vec<(&str, String)> = messages
        .iter()
        .filter_map(|m| {
            let role = m.role()?;
            if role != "user" && role != "assistant" {
                return None;
            }
            let text = crate::sessions::summary::content_to_text(m.content());
            (!text.is_empty()).then_some((role, text))
        })
        .collect();
    let Some((_, first_message)) = turns.first() else {
        return TitleContext {
            first_message: String::new(),
            recent_messages: String::new(),
        };
    };
    let first_message = first_message.clone();

    let overhead = estimate_tokens(
        &template
            .replace("{firstMessage}", "")
            .replace("{recentMessages}", ""),
    );
    let mut budget = context_size
        - overhead
        - output_allowance
        - TITLE_SAFETY_MARGIN_TOKENS
        - estimate_tokens(&first_message);

    let mut picked: Vec<String> = Vec::new();
    for (role, text) in turns.iter().skip(1).rev() {
        let line = format!("{}: {text}", role_label(role));
        // The `+ 1` is the newline that joins this line to the next.
        let cost = estimate_tokens(&line) + 1;
        if budget - cost < 0 {
            break;
        }
        budget -= cost;
        picked.push(line);
    }
    picked.reverse();

    TitleContext {
        first_message,
        recent_messages: picked.join("\n"),
    }
}

fn fill_template(template: &str, context: &TitleContext) -> String {
    let recent = if context.recent_messages.is_empty() {
        "(none)"
    } else {
        &context.recent_messages
    };
    template
        .replace("{firstMessage}", &context.first_message)
        .replace("{recentMessages}", recent)
}

/// Roughly four characters per token, counted in `char`s. There is no tokenizer
/// here and this only has to be in the right ballpark: it feeds a budget that
/// already carries a safety margin.
fn estimate_tokens(s: &str) -> i64 {
    s.chars().count().div_ceil(4) as i64
}

fn role_label(role: &str) -> &'static str {
    if role == "assistant" {
        "Assistant"
    } else {
        "User"
    }
}

/// Six lexical steps, and the ORDER is the whole content of this function:
/// taking the first line before stripping `<think>` leaves a dangling opening
/// tag, and stripping trailing punctuation before the quotes leaves
/// `"Some title".` intact.
pub fn sanitize(raw: &str) -> String {
    let mut s = think_block().replace_all(raw, "").into_owned();
    // Title-only output: everything after the first line is commentary.
    s = s.split('\n').next().unwrap_or_default().trim().to_string();
    s = title_prefix().replace(&s, "").into_owned();
    s = surrounding_quotes().replace_all(&s, "").into_owned();
    s = trailing_punctuation()
        .replace(&s, "")
        .into_owned()
        .trim()
        .to_string();
    s = whitespace_run().replace_all(&s, " ").into_owned();
    if s.chars().count() > MAX_TITLE_CHARS {
        s = s.chars().take(MAX_TITLE_CHARS).collect::<String>();
        s = s.trim_end().to_string();
    }
    s
}

/// Each pattern is compiled once. A `Regex::new` per title is a needless
/// compile on a path that runs after every first turn.
macro_rules! lazy_regex {
    ($name:ident, $pattern:expr) => {
        // Literal patterns, so a compile failure here is a build-time bug.
        #[allow(clippy::unwrap_used)]
        fn $name() -> &'static Regex {
            static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
            RE.get_or_init(|| Regex::new($pattern).unwrap())
        }
    };
}

lazy_regex!(think_block, r"(?is)<think.*?</think>");
lazy_regex!(title_prefix, r"(?i)^title\s*[:-]\s*");
lazy_regex!(surrounding_quotes, r#"^["'`]+|["'`]+$"#);
lazy_regex!(trailing_punctuation, r"[.!?]+$");
lazy_regex!(whitespace_run, r"\s+");
