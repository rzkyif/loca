//! The single-shot LLM utility routes: `/api/v1/llm/{autocorrect,merge}`.
//!
//! Both are short post-processing calls the Client makes around dictation, and
//! they exist so that **no LLM traffic ever bypasses Core**: the client holds no
//! API key and never talks to a provider itself. They resolve their endpoint
//! through the same resolver and the same vault as the chat path.
//!
//! Both prompts are user-overridable through core settings
//! (`prompts.autocorrectPrompt`, `prompts.mergeTranscriptionPrompt`), each
//! falling back to the schema's default when unset or cleared.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::post;
use axum::Router;

use crate::contract::api::{
    AutocorrectRequest, AutocorrectResponse, MergeTranscriptionRequest, MergeTranscriptionResponse,
};
use crate::error::{validation, Result};
use crate::settings::access::str_setting;
use crate::settings::{schema, Settings, SettingsRecord};
use crate::transport::http::Json;
use crate::transport::middleware::{from_fn_with_state, require_client};
use crate::transport::state::AppContext;

use super::budget::thinking_budget;
use super::single_shot::SingleShotOptions;
use super::types::{LlmRoute, Overrides};
use super::Llm;

/// Both calls sit on the dictation hot path, so they pin a low temperature: the
/// job is to correct and combine text, not to write any.
const UTILITY_TEMPERATURE: f64 = 0.2;

#[derive(Clone)]
struct Utility {
    settings: Settings,
    llm: Arc<Llm>,
}

pub fn routes(ctx: &AppContext, llm: Arc<Llm>) -> Router {
    let state = Utility {
        settings: ctx.settings.clone(),
        llm,
    };
    Router::new()
        .route("/api/v1/llm/autocorrect", post(autocorrect))
        .route("/api/v1/llm/merge", post(merge))
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(state)
}

/// Clean up one transcription.
async fn autocorrect(
    State(service): State<Utility>,
    Json(body): Json<AutocorrectRequest>,
) -> Result<Json<AutocorrectResponse>> {
    // The contract declares a plain `string` with no minimum, so emptiness is
    // the one rule left here: a no-op call, not a round trip to the model.
    if body.text.is_empty() {
        return Err(validation("body must be { text: string }"));
    }
    let settings = service.settings.load().await?;
    let started_at = std::time::Instant::now();
    let endpoint = service
        .llm
        .resolve_endpoint(&settings, LlmRoute::Default)
        .await?;
    tracing::info!(
        target: "loca::llm-utility",
        "autocorrect starting (in {} chars, model {})",
        body.text.len(),
        endpoint.model,
    );
    let text = service
        .llm
        .single_shot(SingleShotOptions {
            system_prompt: prompt(&settings, "prompts.autocorrectPrompt"),
            user_message: body.text.clone(),
            endpoint,
            overrides: Overrides {
                temperature: Some(UTILITY_TEMPERATURE),
                // On the dictation hot path, where thinking costs seconds for
                // little gain, so the budget defaults to 0 (off); a positive
                // one opts in and caps the `<think>` block.
                reasoning_budget: Some(thinking_budget(
                    &settings,
                    "prompts.autocorrectThinkingBudget",
                )),
                ..Overrides::default()
            },
        })
        .await?;
    tracing::info!(
        target: "loca::llm-utility",
        "autocorrect done in {}ms (in {} chars, out {} chars)",
        started_at.elapsed().as_millis(),
        body.text.len(),
        text.len(),
    );
    Ok(Json(AutocorrectResponse { text }))
}

/// Merge a new transcription into what is already in the input box.
async fn merge(
    State(service): State<Utility>,
    Json(body): Json<MergeTranscriptionRequest>,
) -> Result<Json<MergeTranscriptionResponse>> {
    // `existing` may legitimately be empty (the box was blank); `next` may not,
    // since there would be nothing to merge.
    if body.next.is_empty() {
        return Err(validation(
            "body must be { existing: string, next: string }",
        ));
    }
    let settings = service.settings.load().await?;
    let user_message = format!(
        "<existing>\n{}\n</existing>\n<new>\n{}\n</new>",
        body.existing, body.next
    );
    let started_at = std::time::Instant::now();
    let endpoint = service
        .llm
        .resolve_endpoint(&settings, LlmRoute::Default)
        .await?;
    tracing::info!(
        target: "loca::llm-utility",
        "merge starting (in {} chars, model {})",
        user_message.len(),
        endpoint.model,
    );
    let sent = user_message.len();
    let text = service
        .llm
        .single_shot(SingleShotOptions {
            system_prompt: prompt(&settings, "prompts.mergeTranscriptionPrompt"),
            user_message,
            endpoint,
            overrides: Overrides {
                temperature: Some(UTILITY_TEMPERATURE),
                // Same hot path as autocorrect: thinking off by default via the
                // Merge Thinking Budget setting, opt in with a positive budget.
                reasoning_budget: Some(thinking_budget(
                    &settings,
                    "prompts.mergeTranscriptionThinkingBudget",
                )),
                ..Overrides::default()
            },
        })
        .await?;
    tracing::info!(
        target: "loca::llm-utility",
        "merge done in {}ms (in {sent} chars, out {} chars)",
        started_at.elapsed().as_millis(),
        text.len(),
    );
    Ok(Json(MergeTranscriptionResponse { text }))
}

/// The user's prompt for `key`, or the schema's default. An empty stored value
/// means the field was cleared, which reads as "use the default": no system
/// prompt at all would turn the utility call into an open-ended completion.
fn prompt(settings: &SettingsRecord, key: &str) -> String {
    let configured = str_setting(settings, key, "");
    if !configured.is_empty() {
        return configured;
    }
    str_setting(schema().defaults(), key, "")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::llm::testing::ScriptedProvider;
    use crate::llm::{ChatProvider, ProviderError};
    use crate::transport::testing;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt as _;

    struct Fixture {
        router: Router,
        provider: ScriptedProvider,
        settings: Settings,
    }

    fn fixture_with(provider: ScriptedProvider) -> Fixture {
        let ctx = testing::context_with(std::sync::Arc::new(
            testing::StubAuth::new().with_client("token", "client-1"),
        ));
        // The context roots a tempdir but does not create it, and a settings
        // PATCH writes a real file.
        std::fs::create_dir_all(ctx.paths.root()).unwrap();
        let llm = Arc::new(crate::llm::Llm::with_provider(
            ctx.vault.clone(),
            ctx.config.channel,
            Arc::new(provider.clone()) as Arc<dyn ChatProvider>,
        ));
        Fixture {
            router: routes(&ctx, llm),
            provider,
            settings: ctx.settings.clone(),
        }
    }

    fn fixture() -> Fixture {
        fixture_with(ScriptedProvider::text("corrected text"))
    }

    impl Fixture {
        async fn post_as(&self, path: &str, body: Value, bearer: bool) -> (StatusCode, Value) {
            let mut request = Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json");
            if bearer {
                request = request.header("authorization", "Bearer token");
            }
            let response = self
                .router
                .clone()
                .oneshot(request.body(Body::from(body.to_string())).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }

        async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
            self.post_as(path, body, true).await
        }

        /// The messages of the `n`th request the provider was handed.
        fn messages(&self, n: usize) -> Value {
            serde_json::to_value(&self.provider.requests()[n].messages).unwrap()
        }

        async fn set_prompt(&self, value: &str) {
            let Value::Object(map) = json!({ "prompts.autocorrectPrompt": value }) else {
                unreachable!()
            };
            self.settings.patch(map).await.unwrap();
        }
    }

    #[tokio::test]
    async fn both_routes_require_a_paired_client() {
        let fixture = fixture();
        for path in ["/api/v1/llm/autocorrect", "/api/v1/llm/merge"] {
            let (status, body) = fixture.post_as(path, json!({}), false).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(body["error"]["code"], json!("missing_token"), "{path}");
        }
    }

    #[tokio::test]
    async fn autocorrect_rejects_a_missing_empty_or_wrong_typed_text() {
        let fixture = fixture();
        for body in [json!({}), json!({ "text": "" }), json!({ "text": 42 })] {
            let (status, response) = fixture.post("/api/v1/llm/autocorrect", body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(
                response["error"]["code"],
                json!("validation_error"),
                "{body}"
            );
        }
        assert!(
            fixture.provider.requests().is_empty(),
            "a rejected body must never reach the model",
        );
    }

    #[tokio::test]
    async fn merge_rejects_a_missing_or_empty_next_but_allows_an_empty_existing() {
        let fixture = fixture();
        for body in [
            json!({ "existing": "x" }),
            json!({ "existing": "x", "next": "" }),
            json!({ "next": "y" }),
        ] {
            let (status, response) = fixture.post("/api/v1/llm/merge", body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(
                response["error"]["code"],
                json!("validation_error"),
                "{body}"
            );
        }
        let (status, _) = fixture
            .post("/api/v1/llm/merge", json!({ "existing": "", "next": "y" }))
            .await;
        assert_eq!(status, StatusCode::OK, "a blank input box is not an error");
    }

    #[tokio::test]
    async fn autocorrect_returns_the_completion_and_sends_the_default_prompt() {
        let fixture = fixture();
        let (status, body) = fixture
            .post("/api/v1/llm/autocorrect", json!({ "text": "helo wrld" }))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "text": "corrected text" }));

        let sent = fixture.provider.requests();
        assert_eq!(sent.len(), 1);
        let messages = fixture.messages(0);
        assert_eq!(
            messages[1],
            json!({ "role": "user", "content": "helo wrld" })
        );
        let system = messages[0]["content"].as_str().unwrap();
        assert!(
            system.starts_with("You are a transcription corrector."),
            "the schema default is the prompt when the user set none: {system}",
        );
        assert_eq!(sent[0].overrides.temperature, Some(0.2));
        // The Cleanup Thinking Budget defaults to 0, which is thinking OFF
        // rather than an unbounded budget.
        assert_eq!(sent[0].overrides.reasoning_budget, Some(0));
    }

    #[tokio::test]
    async fn merge_wraps_the_two_texts_in_the_template_the_prompt_describes() {
        let fixture = fixture_with(ScriptedProvider::text("merged"));
        let (status, body) = fixture
            .post(
                "/api/v1/llm/merge",
                json!({ "existing": "one", "next": "two" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "text": "merged" }));
        assert_eq!(
            fixture.messages(0)[1]["content"],
            json!("<existing>\none\n</existing>\n<new>\ntwo\n</new>"),
            "the tags are what the merge prompt tells the model to expect",
        );
    }

    #[tokio::test]
    async fn a_configured_prompt_wins_and_a_cleared_one_falls_back() {
        let fixture = fixture();
        fixture.set_prompt("MINE").await;
        fixture
            .post("/api/v1/llm/autocorrect", json!({ "text": "x" }))
            .await;
        assert_eq!(fixture.messages(0)[0]["content"], json!("MINE"));

        fixture.set_prompt("").await;
        fixture
            .post("/api/v1/llm/autocorrect", json!({ "text": "x" }))
            .await;
        assert!(
            fixture.messages(1)[0]["content"]
                .as_str()
                .unwrap()
                .starts_with("You are a transcription corrector."),
            "a cleared prompt means the default, not no prompt at all",
        );
    }

    #[tokio::test]
    async fn an_unreachable_provider_answers_500_internal_error() {
        // Unclassified: the chat path classifies instead, and these two routes
        // deliberately do not.
        let fixture = fixture_with(ScriptedProvider::failing(ProviderError::transport(
            "http://127.0.0.1:7701/v1 is unreachable: connection refused",
        )));
        let (status, body) = fixture
            .post("/api/v1/llm/autocorrect", json!({ "text": "x" }))
            .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], json!("internal_error"));
    }
}
