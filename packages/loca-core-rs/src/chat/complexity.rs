//! Dual-model routing: which endpoint a turn runs on when the client did not
//! pin one. `route` on the `chat.start` schema must stay genuinely optional, or
//! the gate here (`!frame.route`) is dead for every frame ever sent. A client
//! that pins a route still gets it verbatim.

use tokio_util::sync::CancellationToken;

use crate::llm::{Llm, LlmRoute, Overrides, SingleShotOptions};
use crate::settings::access::str_setting;
use crate::settings::SettingsRecord;

/// The answer's own allowance, before the thinking budget is added on top. The
/// classifier is asked for one word, so this is deliberately tiny.
const CLASSIFIER_MAX_TOKENS: u32 = 16;

/// Which endpoint this request should run on. Anything not unambiguously
/// "complex" routes to the default, so a confused or empty answer degrades to
/// the normal model rather than the expensive one. `settings` is the RESOLVED
/// record so the prompt and budget carry their schema defaults;
/// `endpoint_settings` is the sparse one. See `turn.rs` for why they differ.
pub async fn classify_complexity(
    llm: &Llm,
    settings: &SettingsRecord,
    endpoint_settings: &SettingsRecord,
    user_message: &str,
    cancel: &CancellationToken,
) -> LlmRoute {
    let system_prompt = str_setting(settings, "prompts.complexityDetectionPrompt", "");
    let Ok(endpoint) = llm
        .resolve_endpoint(endpoint_settings, LlmRoute::Default)
        .await
    else {
        return LlmRoute::Default;
    };
    // Thinking is off by default for this call (schema default 0): the answer is
    // one word. A positive budget opts in and is added ON TOP of the one-word
    // allowance so reasoning cannot crowd the answer out.
    let budget = crate::llm::thinking_budget(settings, "prompts.complexityDetectionThinkingBudget");
    let call = llm.single_shot(SingleShotOptions {
        system_prompt,
        user_message: user_message.to_string(),
        endpoint,
        overrides: Overrides {
            temperature: Some(0.0),
            max_tokens: Some(CLASSIFIER_MAX_TOKENS + budget),
            reasoning_budget: Some(budget),
            ..Overrides::default()
        },
    });
    // An interrupt during classification is a turn that is already over, and
    // the caller checks the token again before hop 0.
    let answer = tokio::select! {
        biased;
        _ = cancel.cancelled() => return LlmRoute::Default,
        answer = call => answer,
    };
    match answer {
        Ok(text) => route_of(&text),
        Err(err) => {
            tracing::warn!(
                target: "loca::chat",
                "complexity classifier failed; defaulting to \"default\": {err}",
            );
            LlmRoute::Default
        }
    }
}

/// `secondary` only when the answer names complexity and does NOT also name
/// simplicity. A reply containing both ("not complex, simple") is ambiguous and
/// takes the default.
pub(super) fn route_of(answer: &str) -> LlmRoute {
    let text = answer.to_lowercase();
    if text.contains("complex") && !text.contains("simple") {
        LlmRoute::Secondary
    } else {
        LlmRoute::Default
    }
}
