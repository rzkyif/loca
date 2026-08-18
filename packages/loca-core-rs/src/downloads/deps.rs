//! What the download engine needs from the rest of the core.
//!
//! One narrow trait, following the `deps.rs` pattern D9 established and D6
//! reused: the fact lives with the package that owns it, and this package holds
//! only the question.

use std::sync::Arc;

use async_trait::async_trait;

/// The sha256 the **signed** model catalog pins for a source spec.
///
/// Anchor two of the transfer's three. It matters that it is the signed catalog
/// rather than a header: it holds even when a proxy strips HuggingFace's etag,
/// and it is the only anchor that survives the file moving to a mirror.
#[async_trait]
pub trait CatalogHashes: Send + Sync {
    /// `None` when the spec is not a catalog file, which is the common case: a
    /// user-supplied repo file has no pinned hash anywhere.
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

/// The stub behind the catalog seam.
///
/// `None` is the honest answer rather than a degradation: a core with no catalog
/// has no pinned hash, and the transfer then falls through to HuggingFace's own
/// published sha256, which is already the normal path for any file the catalog
/// does not list.
pub struct NoCatalog;

#[async_trait]
impl CatalogHashes for NoCatalog {
    async fn catalog_sha256(&self, _spec: &str) -> Option<String> {
        None
    }
}
