//! The seam: the in-process engine as one more [`ChatProvider`].
//!
//! This is the whole of how local inference is wired in. D3 built
//! `Llm::set_local_provider` for exactly this and nothing calls it yet;
//! `Llm::provider_for` selects it when the endpoint carries
//! `is_local_provider`, and falls back to `HttpProvider` when it is not
//! installed.
//!
//! **No SSE.** Server-sent events are the external path's business: the engine
//! emits [`crate::llm::LlmDelta`] directly, with no encoder and no decoder in
//! between. The obligation that replaces the wire format is a property rather
//! than a shape, and `super::diff` tests it: the delta sequence emitted here
//! must be indistinguishable from what `llm::sse::SseDecoder` plus
//! `ChatChunk::to_delta` would have produced from `llama-server`'s stream.

use std::sync::Arc;

use crate::llm::{ChatProvider, DeltaStream, LlmRequest};

use super::engine::LocalEngine;
use super::runtime;

/// One turn against the in-process engine.
///
/// A thin `Arc` around the engine rather than the engine itself, because the
/// same engine is also the `ModelIdle` and `EnsureModelLoaded` seam and there is
/// exactly one of it per core: a second would mean a second model in memory.
pub struct LocalProvider {
    engine: Arc<LocalEngine>,
}

impl LocalProvider {
    pub fn new(engine: Arc<LocalEngine>) -> Self {
        Self { engine }
    }
}

impl ChatProvider for LocalProvider {
    fn stream(&self, req: LlmRequest) -> DeltaStream {
        runtime::stream_turn(Arc::clone(&self.engine), req)
    }
}
