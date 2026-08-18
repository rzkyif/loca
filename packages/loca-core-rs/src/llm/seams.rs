//! What the extension supervisor reaches the model through.
//!
//! Both traits are DECLARED by `extensions::broker` (the consumer declares, so
//! nothing under `extensions/` imports an `llm::` type) and implemented here,
//! over the one `Llm` the whole core shares.

use async_trait::async_trait;

use crate::error::Result;
use crate::extensions::broker::{EnsureModelLoaded, ToolCompletion, ToolLlm};
use crate::settings::Settings;

use super::{classify_provider_error, Llm, LlmRoute, Overrides, SingleShotOptions};

/// One short completion on behalf of an extension's tool.
#[derive(Clone)]
pub struct ToolCompletions {
    llm: Llm,
    settings: Settings,
}

impl ToolCompletions {
    pub fn new(llm: Llm, settings: Settings) -> Self {
        Self { llm, settings }
    }
}

#[async_trait]
impl ToolLlm for ToolCompletions {
    async fn complete(&self, req: ToolCompletion) -> Result<String> {
        // The SPARSE record, never `load_resolved`: the resolver's own defaults
        // differ from the schema's for two keys and it applies the channel port
        // offset itself, both of which a resolved record would have already
        // decided wrongly. There is no client on a tool call, so the shared
        // store is the whole of what applies.
        let settings = self.settings.load().await?;
        let endpoint = self
            .llm
            .resolve_endpoint(&settings, LlmRoute::Default)
            .await?;
        self.llm
            .single_shot(SingleShotOptions {
                system_prompt: req.system_prompt,
                user_message: req.user_message,
                endpoint,
                overrides: Overrides {
                    max_tokens: Some(req.max_tokens),
                    reasoning_budget: Some(req.reasoning_budget),
                    ..Overrides::default()
                },
            })
            .await
            .map_err(|err| classify_provider_error(&err))
    }
}

/// Nothing to load.
///
/// This is the CORRECT implementation on a core with no in-process inference,
/// not a stub standing in for one: there is no local engine to bring up, so the
/// completion then succeeds or fails on the endpoint alone, exactly as it does
/// today when an external provider answers or the sidecar is already warm. N2
/// replaces it with the one that warms the local model.
pub struct NoModelToLoad;

#[async_trait]
impl EnsureModelLoaded for NoModelToLoad {
    async fn ensure_loaded(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use crate::config::Channel;
    use crate::llm::testing::ScriptedProvider;
    use crate::secrets::testing::MemoryStore;
    use crate::secrets::Vault;
    use crate::transport::testing;

    use super::*;

    fn completion(max_tokens: u32) -> ToolCompletion {
        ToolCompletion {
            system_prompt: "be brief".to_string(),
            user_message: "label this".to_string(),
            max_tokens,
            reasoning_budget: 0,
        }
    }

    #[tokio::test]
    async fn a_tool_completion_carries_its_cap_and_no_thinking_budget() {
        let ctx = testing::context();
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::with_store(dir.path().join("secrets.enc"), Arc::new(MemoryStore::new()));
        let provider = ScriptedProvider::text("a label");
        let llm = Llm::with_provider(vault, Channel::Stable, Arc::new(provider.clone()));
        let seam = ToolCompletions::new(llm, ctx.settings.clone());

        assert_eq!(seam.complete(completion(64)).await.unwrap(), "a label");

        let seen = provider.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].overrides.max_tokens, Some(64));
        // Zero rather than `None`: `None` leaves the endpoint's own reasoning
        // mode in charge, and a reasoning model would then put its `<think>`
        // text inside the one string the tool acts on.
        assert_eq!(seen[0].overrides.reasoning_budget, Some(0));
        assert!(
            seen[0].tools.is_empty(),
            "a tool's own completion has no tools"
        );
        assert_eq!(seen[0].messages.len(), 2);
    }

    /// A core with no in-process engine has nothing to load, and saying so is
    /// the behaviour rather than a placeholder for it.
    #[tokio::test]
    async fn nothing_to_load_succeeds() {
        NoModelToLoad.ensure_loaded().await.unwrap();
    }
}
