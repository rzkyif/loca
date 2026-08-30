//! LLM provider, endpoint resolution, request scheduling, thinking budget.
//!
//! Three layers: [`provider`] owns the upstream call and normalises it into
//! [`LlmDelta`] behind the [`ChatProvider`] seam; [`resolver`] turns a settings
//! record into the endpoint to call; [`scheduler`] bounds how many local
//! completions run at once, deliberately IN FRONT of the trait so one queue
//! governs every transport. The chat orchestrator reaches all three through
//! [`Llm`].

pub mod budget;
pub mod provider;
pub mod resolver;
pub mod routes;
pub mod scheduler;
pub mod seams;
pub mod single_shot;
pub mod sse;
pub mod types;

#[cfg(any(test, feature = "in-memory"))]
pub mod testing;

use std::sync::{Arc, Mutex};

pub use budget::thinking_budget;
pub use provider::{
    classify_provider_error, ChatProvider, DeltaStream, HttpProvider, ProviderError,
    ProviderErrorKind,
};
pub use resolver::{resolve_context_size, resolve_endpoint};
pub use scheduler::{LlmScheduler, QueueObserver, ScheduleOptions, WatchdogHandler, WatchdogInfo};
pub use seams::{NoModelToLoad, ToolCompletions};
pub use single_shot::{single_shot, SingleShotOptions};
pub use types::{
    ChatMessage, ContentPart, ImageUrl, LlmDelta, LlmEndpointConfig, LlmRequest, LlmRoute,
    Overrides, Reasoning, ReasoningEffort, ToolCallDelta, ToolCallRef, ToolDef, UserContent,
};

use crate::config::Channel;
use crate::error::Result;
use crate::secrets::Vault;
use crate::settings::SettingsRecord;
use crate::transport::status::CoreStatusService;

/// Everything a caller needs to run a completion. Cheap to clone; every clone
/// shares one queue. Exactly one per core: a second would give the utility
/// routes a different queue from the chat turns, and `llm.local.parallelSlots`
/// would bound neither.
#[derive(Clone)]
pub struct Llm {
    inner: Arc<Inner>,
}

struct Inner {
    vault: Vault,
    channel: Channel,
    scheduler: LlmScheduler,
    /// Every external endpoint, and the local one when no engine is installed.
    external: Arc<dyn ChatProvider>,
    /// The in-process engine, when this build has one.
    local: Mutex<Option<Arc<dyn ChatProvider>>>,
}

impl std::fmt::Debug for Llm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Llm")
    }
}

/// What a turn needs beyond its request. Deliberately NOT whether it queues:
/// that is read off the resolved endpoint, so a caller cannot pass the
/// URL-derived locality (which gates the llama sampler fields) where the
/// provider-derived one belongs. The two disagree on a loopback gateway.
pub struct TurnOptions {
    pub client_id: String,
    /// From `llm.local.parallelSlots`, when the caller reads it. `None` leaves
    /// the current setting alone.
    pub parallel_slots: Option<f64>,
}

impl Llm {
    pub fn new(vault: Vault, channel: Channel) -> Self {
        Self::with_provider(vault, channel, Arc::new(HttpProvider::new()))
    }

    /// The same, over a caller-supplied provider. A test injects a scripted one;
    /// production uses [`HttpProvider`].
    pub fn with_provider(vault: Vault, channel: Channel, provider: Arc<dyn ChatProvider>) -> Self {
        Self {
            inner: Arc::new(Inner {
                vault,
                channel,
                scheduler: LlmScheduler::new(),
                external: provider,
                local: Mutex::new(None),
            }),
        }
    }

    /// Install the in-process engine. One call, at boot.
    pub fn set_local_provider(&self, provider: Arc<dyn ChatProvider>) {
        *lock(&self.inner.local) = Some(provider);
    }

    /// Whether the in-process engine is installed. Reads the LIVE field rather
    /// than restating what boot intended: without the install a local turn
    /// silently resolves `HttpProvider` and talks to a port nothing serves.
    pub fn has_local_provider(&self) -> bool {
        lock(&self.inner.local).is_some()
    }

    /// Publish queue depth to the core-status aggregator.
    pub fn attach_status(&self, status: CoreStatusService) {
        self.inner
            .scheduler
            .attach_status(Arc::new(status) as Arc<dyn QueueObserver>);
    }

    /// What to run when a local completion wedges: restart the model.
    pub fn set_watchdog_handler(&self, handler: Option<WatchdogHandler>) {
        self.inner.scheduler.set_watchdog_handler(handler);
    }

    pub fn scheduler(&self) -> &LlmScheduler {
        &self.inner.scheduler
    }

    /// Resolve the endpoint for `route` from a **sparse** settings record.
    /// `Settings::load` or `load_effective`, never `load_resolved`: see
    /// [`resolver`] for the defaults that differ and the channel-adjusted port
    /// that would be lost.
    pub async fn resolve_endpoint(
        &self,
        settings: &SettingsRecord,
        route: LlmRoute,
    ) -> Result<LlmEndpointConfig> {
        resolve_endpoint(settings, &self.inner.vault, self.inner.channel, route).await
    }

    /// Run a completion through the queue.
    ///
    /// Awaiting this is waiting in the queue; dropping the future leaves it
    /// clean. `server_busy` comes back from here rather than from the stream, so
    /// a caller learns it is refused before it starts rendering.
    pub async fn stream(&self, req: LlmRequest, opts: TurnOptions) -> Result<DeltaStream> {
        let provider = self.provider_for(&req.endpoint);
        let is_local = req.endpoint.is_local_provider;
        self.inner
            .scheduler
            .schedule(
                provider.as_ref(),
                req,
                ScheduleOptions {
                    client_id: opts.client_id,
                    is_local,
                    parallel_slots: opts.parallel_slots,
                },
            )
            .await
    }

    /// A raw completion stream, outside the queue, for a utility call that
    /// needs the deltas as they arrive and must not queue behind chat turns.
    /// Title generation is the one caller: it enforces its own deadline and cap
    /// next to the constants that motivate them.
    pub fn stream_unqueued(&self, req: LlmRequest) -> DeltaStream {
        self.provider_for(&req.endpoint).stream(req)
    }

    /// A completion collected into one string, outside the queue.
    pub async fn single_shot(
        &self,
        opts: SingleShotOptions,
    ) -> std::result::Result<String, ProviderError> {
        let provider = self.provider_for(&opts.endpoint);
        single_shot(provider.as_ref(), opts).await
    }

    fn provider_for(&self, endpoint: &LlmEndpointConfig) -> Arc<dyn ChatProvider> {
        if endpoint.is_local_provider {
            if let Some(local) = lock(&self.inner.local).clone() {
                return local;
            }
        }
        Arc::clone(&self.inner.external)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::secrets::testing::MemoryStore;
    use futures_util::stream::StreamExt as _;

    fn vault() -> (Vault, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::with_store(dir.path().join("secrets.enc"), Arc::new(MemoryStore::new()));
        (vault, dir)
    }

    fn llm(provider: Arc<dyn ChatProvider>) -> (Llm, tempfile::TempDir) {
        let (vault, dir) = vault();
        (Llm::with_provider(vault, Channel::Stable, provider), dir)
    }

    fn request(endpoint: LlmEndpointConfig) -> LlmRequest {
        LlmRequest::new(endpoint, vec![ChatMessage::user("hi")])
    }

    #[tokio::test]
    async fn a_loopback_gateway_that_is_not_the_local_provider_does_not_queue() {
        // The two-localities trap again: deriving "does this queue" from
        // `llama_body_extras` instead of `is_local_provider` puts every
        // external loopback gateway behind one slot, which is a hang.
        let (llm, _dir) = llm(Arc::new(testing::PendingProvider::new()));
        llm.scheduler().set_parallel_slots(1.0).unwrap();
        let endpoint = LlmEndpointConfig {
            base_url: "http://127.0.0.1:1234/v1".into(),
            llama_body_extras: true,
            is_local_provider: false,
            ..LlmEndpointConfig::default()
        };
        let mut streams = Vec::new();
        for _ in 0..5 {
            // Bounded rather than a bare await: if this regressed to queueing
            // the fifth would wait forever, and a hanging test is no tripwire.
            let started = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                llm.stream(
                    request(endpoint.clone()),
                    TurnOptions {
                        client_id: "c".into(),
                        parallel_slots: None,
                    },
                ),
            )
            .await
            .expect("an external gateway never queues, however local its URL looks");
            streams.push(started.unwrap());
        }
        assert_eq!(llm.scheduler().depth(), (0, 0));
    }

    #[tokio::test]
    async fn the_local_provider_is_used_for_the_local_endpoint_once_installed() {
        let external = testing::ScriptedProvider::text("from external");
        let local = testing::ScriptedProvider::text("from local");
        let (llm, _dir) = llm(Arc::new(external.clone()));
        llm.set_local_provider(Arc::new(local.clone()));

        let text = |endpoint: LlmEndpointConfig| {
            let llm = llm.clone();
            async move {
                let stream = llm
                    .stream(
                        request(endpoint),
                        TurnOptions {
                            client_id: "c".into(),
                            parallel_slots: None,
                        },
                    )
                    .await
                    .unwrap();
                stream
                    .filter_map(|item| async move { item.ok().and_then(|d| d.content_delta) })
                    .collect::<Vec<_>>()
                    .await
                    .join("")
            }
        };

        assert_eq!(
            text(LlmEndpointConfig {
                is_local_provider: true,
                ..LlmEndpointConfig::default()
            })
            .await,
            "from local",
        );
        assert_eq!(
            text(LlmEndpointConfig {
                is_local_provider: false,
                ..LlmEndpointConfig::default()
            })
            .await,
            "from external",
        );
        assert_eq!(local.requests().len(), 1);
        assert_eq!(external.requests().len(), 1);
    }
}
