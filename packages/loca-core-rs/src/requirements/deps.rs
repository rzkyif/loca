//! The seam onto the binary installer. `probe` reaches GitHub, so tests need a
//! fake to cover the size-hint path and the consent assertion.

use async_trait::async_trait;

use crate::contract::api::{BinaryKind, BinaryProbeResult, BinaryStatus};
use crate::downloads::binaries::installer::BinariesInstaller;
use crate::downloads::cancel::CancellationToken;
use crate::error::Result;

/// The binary installer, as the requirements snapshot uses it.
#[async_trait]
pub trait Binaries: Send + Sync {
    /// Install state per kind. Network-free, so the always-on snapshot may call it.
    async fn list(&self) -> Result<Vec<BinaryStatus>>;

    /// Download size and resolved version for the confirm modal. **Reaches
    /// upstream**, so it is called only on the consented `?probe=1` path.
    async fn probe(&self, kinds: &[BinaryKind]) -> Result<Vec<BinaryProbeResult>>;

    /// Why the last install attempt for `kind` failed, if it did.
    fn failure(&self, kind: BinaryKind) -> Option<String>;

    /// Install the named kinds, answering one queue-row id per kind that
    /// started. Never called with an empty list.
    async fn install(&self, kinds: Vec<BinaryKind>) -> Result<Vec<String>>;
}

#[async_trait]
impl Binaries for BinariesInstaller {
    async fn list(&self) -> Result<Vec<BinaryStatus>> {
        BinariesInstaller::list(self).await
    }

    async fn probe(&self, kinds: &[BinaryKind]) -> Result<Vec<BinaryProbeResult>> {
        // A fresh token per call: one shared across calls would let an
        // abandoned probe cancel the next.
        BinariesInstaller::probe(self, kinds, &CancellationToken::new()).await
    }

    fn failure(&self, kind: BinaryKind) -> Option<String> {
        BinariesInstaller::failure(self, kind)
    }

    async fn install(&self, kinds: Vec<BinaryKind>) -> Result<Vec<String>> {
        // `None` means "everything missing"; this caller already chose.
        BinariesInstaller::install(self, Some(kinds)).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::Channel;
    use crate::transport::testing::context;

    /// A real installer over an unwired queue; every method reached is network-free.
    fn installer() -> BinariesInstaller {
        let ctx = context();
        let net = Arc::new(crate::downloads::net::Net::new().unwrap());
        BinariesInstaller::new(
            Arc::clone(&net),
            crate::downloads::queue::DownloadQueue::new(
                crate::downloads::store::DownloadStore::new(ctx.db.clone(), ""),
                Arc::new(crate::downloads::transfer::Transfer::new(
                    net,
                    Arc::new(crate::downloads::deps::NoCatalog),
                    "",
                )),
                (*ctx.paths).clone(),
                "",
            ),
            (*ctx.paths).clone(),
            Channel::Stable,
            "",
        )
    }

    #[tokio::test]
    async fn the_seam_forwards_to_the_installer_rather_than_answering_for_it() {
        let binaries: Arc<dyn Binaries> = Arc::new(installer());

        let listed = binaries.list().await.unwrap();
        assert_eq!(listed.len(), 2, "{listed:?}");
        let deno = listed
            .iter()
            .find(|s| s.kind == BinaryKind::Deno)
            .expect("the worker runtime is listed");
        assert!(!deno.installed, "nothing is on disk in a fresh temp root");
        assert!(deno.desired_variant.is_some());
        assert!(listed
            .iter()
            .filter(|s| s.kind != BinaryKind::Deno)
            .all(|s| s.desired_variant.is_none()));

        assert_eq!(binaries.failure(BinaryKind::Deno), None);
    }
}
