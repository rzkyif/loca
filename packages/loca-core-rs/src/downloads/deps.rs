//! What the download engine needs from the rest of the core.

use std::sync::Arc;

use async_trait::async_trait;

/// The sha256 the **signed** model catalog pins for a source spec. Signed
/// rather than a header, so it holds when a proxy strips HuggingFace's etag.
#[async_trait]
pub trait CatalogHashes: Send + Sync {
    /// `None` when the spec is not a catalog file.
    async fn catalog_sha256(&self, spec: &str) -> Option<String>;
}

/// Everything the transfer needs from elsewhere.
#[derive(Clone)]
pub struct DownloadsDeps {
    pub catalog: Arc<dyn CatalogHashes>,
}

impl DownloadsDeps {
    pub fn unwired() -> Self {
        Self {
            catalog: Arc::new(NoCatalog),
        }
    }

    pub fn with_catalog(mut self, catalog: Arc<dyn CatalogHashes>) -> Self {
        self.catalog = catalog;
        self
    }
}

impl Default for DownloadsDeps {
    fn default() -> Self {
        Self::unwired()
    }
}

/// The stub behind the catalog seam: no pinned hash, so the transfer falls
/// through to HuggingFace's own published sha256.
pub struct NoCatalog;

#[async_trait]
impl CatalogHashes for NoCatalog {
    async fn catalog_sha256(&self, _spec: &str) -> Option<String> {
        None
    }
}
