//! A completion collected into one string.
//!
//! For the utility calls that want an answer rather than a stream: the two
//! transcription routes here, title generation and the complexity router (D4),
//! the memory summariser (D5), and the extension module broker (D12).
//!
//! It deliberately does NOT go through the scheduler. These calls are short and
//! parallel-safe, and queueing them behind chat turns would make a dictation
//! cleanup wait for a long generation to finish. If one of them ever needs the
//! slot discipline, it can ask for it explicitly.

use futures_util::stream::StreamExt as _;

use super::provider::{ChatProvider, ProviderError};
use super::types::{ChatMessage, LlmEndpointConfig, LlmRequest, Overrides};

pub struct SingleShotOptions {
    pub system_prompt: String,
    pub user_message: String,
    pub endpoint: LlmEndpointConfig,
    pub overrides: Overrides,
}

/// Run the completion and return its assistant content, trimmed.
///
/// Reasoning is discarded: a caller that wanted the trace would want the stream.
pub async fn single_shot(
    provider: &dyn ChatProvider,
    opts: SingleShotOptions,
) -> Result<String, ProviderError> {
    let request = LlmRequest::new(
        opts.endpoint,
        vec![
            ChatMessage::system(opts.system_prompt),
            ChatMessage::user(opts.user_message),
        ],
    )
    .with_overrides(opts.overrides);
    let mut stream = provider.stream(request);
    let mut text = String::new();
    while let Some(item) = stream.next().await {
        if let Some(delta) = item?.content_delta {
            text.push_str(&delta);
        }
    }
    Ok(text.trim().to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::llm::testing::ScriptedProvider;

    fn options(endpoint: LlmEndpointConfig) -> SingleShotOptions {
        SingleShotOptions {
            system_prompt: "be brief".into(),
            user_message: "helo wrld".into(),
            endpoint,
            overrides: Overrides {
                temperature: Some(0.2),
                reasoning_budget: Some(0),
                ..Overrides::default()
            },
        }
    }

    #[tokio::test]
    async fn the_content_is_concatenated_and_trimmed() {
        let provider = ScriptedProvider::text("  hello world  ");
        let text = single_shot(&provider, options(LlmEndpointConfig::default()))
            .await
            .unwrap();
        assert_eq!(text, "hello world");
    }

    #[tokio::test]
    async fn the_system_prompt_and_the_user_message_are_the_only_two_turns() {
        let provider = ScriptedProvider::text("ok");
        single_shot(&provider, options(LlmEndpointConfig::default()))
            .await
            .unwrap();
        let sent = provider.requests();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            serde_json::to_value(&sent[0].messages).unwrap(),
            serde_json::json!([
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "helo wrld" },
            ])
        );
        assert_eq!(sent[0].overrides.temperature, Some(0.2));
        assert_eq!(sent[0].overrides.reasoning_budget, Some(0));
    }

    #[tokio::test]
    async fn a_provider_failure_propagates_rather_than_returning_empty_text() {
        // An empty string is indistinguishable from a model that had nothing to
        // say, so the caller has to be able to tell the difference.
        let provider = ScriptedProvider::failing(ProviderError::transport("connection refused"));
        let err = single_shot(&provider, options(LlmEndpointConfig::default()))
            .await
            .unwrap_err();
        assert_eq!(err.message, "connection refused");
    }
}
