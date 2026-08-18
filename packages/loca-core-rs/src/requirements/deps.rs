//! The seam onto the binary installer, which is D7's.
//!
//! Declared here and implemented for D7's concrete type in the same file,
//! rather than asked of D7: nothing about the installer needs to change, and a
//! trait plus a four-method forwarding impl is cheaper than a cross-package
//! edit. It follows `storage/deps.rs` and `models/deps.rs` in shape.
//!
//! # Why a seam at all, when `BinariesInstaller` is constructible in a test
//!
//! Two of its four methods reach upstream. `probe` resolves a GitHub release
//! for a resolver entry, which every entry on this core is, so a test over the
//! real type would either hit `api.github.com` or assert around a failure. The
//! seam is what lets the size-hint path and the consent assertion be tested at
//! all.
//!
//! The models side needs no equivalent, because `Models` already carries D6's
//! `Downloads` seam and a test injects a fake through
//! `ModelsDeps::with_downloads`.

use async_trait::async_trait;

use crate::contract::api::{BinaryKind, BinaryProbeResult, BinaryStatus};
use crate::downloads::binaries::installer::BinariesInstaller;
use crate::downloads::cancel::CancellationToken;
use crate::error::Result;

/// The binary installer, as the requirements snapshot uses it.
#[async_trait]
pub trait Binaries: Send + Sync {
    /// Install state per kind. Network-free (D-061), which is what lets the
    /// always-on snapshot call it.
    async fn list(&self) -> Result<Vec<BinaryStatus>>;

    /// Download size and resolved version for the confirm modal. **Reaches
    /// upstream**, so it is called only on the consented `?probe=1` path.
    async fn probe(&self, kinds: &[BinaryKind]) -> Result<Vec<BinaryProbeResult>>;

    /// Why the last install attempt for `kind` failed, if it did.
    fn failure(&self, kind: BinaryKind) -> Option<String>;

    /// Install the named kinds, answering one queue-row id per kind that
    /// started. Never called with an empty list; see the route.
    async fn install(&self, kinds: Vec<BinaryKind>) -> Result<Vec<String>>;
}

#[async_trait]
impl Binaries for BinariesInstaller {
    async fn list(&self) -> Result<Vec<BinaryStatus>> {
        BinariesInstaller::list(self).await
    }

    async fn probe(&self, kinds: &[BinaryKind]) -> Result<Vec<BinaryProbeResult>> {
        // A fresh token per call rather than one threaded from the request:
        // the oracle's probe has no cancellation either, and a token shared
        // across calls would let one abandoned probe cancel the next.
        BinariesInstaller::probe(self, kinds, &CancellationToken::new()).await
    }

    fn failure(&self, kind: BinaryKind) -> Option<String> {
        BinariesInstaller::failure(self, kind)
    }

    async fn install(&self, kinds: Vec<BinaryKind>) -> Result<Vec<String>> {
        // `Some`, always: `None` means "everything missing" (D-028) and this
        // caller has already decided exactly which kinds it wants.
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

    /// A real installer over an unwired queue, following `Services::unwired`.
    /// Every method reached below is network-free.
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

    /// The forwarder reaches the REAL methods, which a four-line impl can get
    /// wrong silently: a `list` wired to something else would answer a plausible
    /// empty vec and every binary would read as missing forever.
    ///
    /// Driven through `dyn Binaries`, because that is the only form the snapshot
    /// holds it in.
    #[tokio::test]
    async fn the_seam_forwards_to_the_installer_rather_than_answering_for_it() {
        let binaries: Arc<dyn Binaries> = Arc::new(installer());

        // `list` enumerates every kind on the wire contract, not just the one
        // this core installs: the client renders a row per kind.
        let listed = binaries.list().await.unwrap();
        assert_eq!(listed.len(), 3, "{listed:?}");
        let deno = listed
            .iter()
            .find(|s| s.kind == BinaryKind::Deno)
            .expect("the worker runtime is listed");
        assert!(!deno.installed, "nothing is on disk in a fresh temp root");
        // Only the installable kind advertises a desired variant, which is the
        // narrowing this core carries; a forwarder answering from somewhere else
        // would not reproduce it.
        assert!(deno.desired_variant.is_some());
        assert!(listed
            .iter()
            .filter(|s| s.kind != BinaryKind::Deno)
            .all(|s| s.desired_variant.is_none()));

        // `failure` reads the installer's own map, which is empty until an
        // install has failed.
        assert_eq!(binaries.failure(BinaryKind::Deno), None);
    }
}
