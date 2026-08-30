//! Consent-gated downloads: resume by Range, streaming sha256, stall watchdog, one-at-a-time queue.
//!
//! Every outbound fetch a running core makes for weights or the pinned `deno`
//! goes through here, and [`net::Net`] is the one chokepoint: no other file in
//! this module may name the HTTP crate.
//!
//! Boot calls two things, in this order: [`queue::DownloadQueue::start`]
//! (normalise the table against the disk, respawn the rows that were mid-flight)
//! then [`binaries::installer::BinariesInstaller::reconcile_interrupted_installs`]
//! (finish the `binaries` rows `start` deliberately skips).

pub mod binaries;
pub mod cancel;
pub mod deps;
pub mod models_seam;
pub mod net;
pub mod queue;
pub mod sources;
pub mod store;
pub mod transfer;

#[cfg(test)]
pub mod testing;

use crate::contract::api::DownloadEntryDestination;

/// One file to fetch.
///
/// `url`, `sha256` and `rel_path` are read as "present", not "truthy": an empty
/// `url` is refused rather than replaced by the parsed source, an empty `sha256`
/// shadows the catalog and disables verification and resume, and an empty
/// `rel_path` resolves the destination root itself.
#[derive(Clone, Debug)]
pub struct EnqueueSpec {
    /// `@user/repo/branch/file`, or a bare path for something already on disk.
    pub source: String,
    pub destination: DownloadEntryDestination,
    /// Groups the rows a single user action created, so the UI can show one
    /// progress bar for a model that is three files.
    pub group_id: String,
    pub size_hint: Option<u64>,
    /// Lowercase hex sha256, verified after the last byte. Pinned for binaries;
    /// usually absent for models, which HuggingFace publishes a hash for.
    pub sha256: Option<String>,
    /// Direct-URL mode: bypasses the source parser, using this URL plus
    /// `rel_path`. How a binary is fetched from the Loca CDN.
    pub url: Option<String>,
    pub rel_path: Option<String>,
    pub filename: Option<String>,
}
