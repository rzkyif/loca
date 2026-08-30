//! The in-process engine as one more [`ChatProvider`]. `Llm::provider_for`
//! selects it when the endpoint carries `is_local_provider`.

use std::sync::Arc;

use crate::llm::{ChatProvider, DeltaStream, LlmRequest};

use super::engine::LocalEngine;
use super::runtime;

/// `Arc` because the one engine per core is also the `ModelIdle` and
/// `EnsureModelLoaded` seam.
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
