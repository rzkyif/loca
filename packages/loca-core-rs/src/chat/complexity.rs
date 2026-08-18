//! Dual-model routing: which endpoint a turn runs on when the client did not
//! pin one.
//!
//! Ports `core-engine/src/services/chat-complexity-router.ts` in full.
//!
//! # This path is live, and was not
//!
//! In the shipping TypeScript this classifier has never run once. `route` on the
//! `chat.start` schema carried a `.default("default")`, Zod applied it during
//! parse, and the orchestrator's gate is `!frame.route`, so the branch was dead
//! for every frame ever sent (D-058). The user's ruling (D-066) was to drop the
//! default rather than delete the feature, so `route` is genuinely optional in
//! both cores and an absent field reaches here.
//!
//! It is therefore wired as reachable and tested as reachable. Nothing else
//! about the turn changes: a client that pins a route still gets it verbatim.

use tokio_util::sync::CancellationToken;

use crate::llm::{Llm, LlmRoute, Overrides, SingleShotOptions};
use crate::settings::access::str_setting;
use crate::settings::SettingsRecord;

/// The answer's own allowance, before the thinking budget is added on top. The
/// classifier is asked for one word, so this is deliberately tiny.
const CLASSIFIER_MAX_TOKENS: u32 = 16;

/// Which endpoint this request should run on.
///
/// Anything the model says that is not unambiguously "complex" routes to the
/// default, so a confused or empty answer degrades to the normal model rather
/// than to the expensive one. That asymmetry is the TypeScript's and is kept:
/// the failure that costs the user money is the one worth biasing against.
/// `settings` is the RESOLVED record, so the prompt and the budget carry their
/// schema defaults; `endpoint_settings` is the sparse one, because the endpoint
/// resolver's fallbacks are its own. See `turn.rs` for why the two differ.
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
    // An interrupt during classification is a turn that is already over. The
    // TypeScript's catch treats an aborted classifier as a failure and falls
    // back, and the caller checks the token again before hop 0, so returning the
    // default here loses nothing.
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
