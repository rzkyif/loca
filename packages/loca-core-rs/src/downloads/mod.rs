//! Consent-gated downloads: resume by Range, streaming sha256, stall watchdog, one-at-a-time queue.
//!
//! Work package **D7** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! # The consent rule lives here
//!
//! `AGENTS.md`: once installed, Core makes NO outbound request without an
//! explicit user action. After Stage 3 absorbs the sidecars, model weights plus
//! one pinned `deno` are the whole consent-gated surface, and all of it comes
//! through this module. [`net::Net`] is the one chokepoint, it counts every call
//! it makes, and no other file here is allowed to name the HTTP crate.
//!
//! # Layout
//!
//! | file            | what it owns                                                   |
//! | --------------- | -------------------------------------------------------------- |
//! | `net.rs`        | the HTTP clients, the three verbs, the call counter            |
//! | `cancel.rs`     | the cooperative stop shared by a transfer and its caller        |
//! | `sources.rs`    | `@user/repo/branch/file` parsing, the safe-component check, the size probe |
//! | `store.rs`      | the `downloads` table: every read, every write, the row mapping |
//! | `transfer.rs`   | Range resume, streaming sha256, the stall watchdog, the ceiling |
//! | `queue.rs`      | one transfer at a time, the in-flight set, the waiter fanout    |
//! | `deps.rs`       | what this package needs from elsewhere, and its unwired stub    |
//! | `models_seam.rs`| `models::deps::Downloads`, the sync trait over the async queue  |
//! | `binaries/`     | `binaries.json`, upstream resolution, the installer, the routes |
//!
//! # Boot
//!
//! Two calls, in this order, and the order is load-bearing:
//! [`queue::DownloadQueue::start`] (normalise the table against the disk, then
//! respawn the rows that were mid-flight) and
//! [`binaries::installer::BinariesInstaller::reconcile_interrupted_installs`]
//! (finish the `binaries` rows `start` deliberately skips). See their docs.

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
/// Three of its fields are read with "present", not "truthy", and the difference
/// is behaviour rather than style:
///
/// - `url: Some("")` is a caller that bypassed the source parser and named
///   nothing, so it is refused rather than silently replaced by the parsed
///   source's URL;
/// - `sha256: Some("")` shadows the catalog and the etag lookup (neither is
///   consulted) and then disables verification and resume;
/// - `rel_path: Some("")` resolves the destination root itself.
///
/// All three are the oracle's, traced branch by branch rather than tidied.
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
    /// Direct-URL mode: when set, the source parser is bypassed and this URL
    /// plus `rel_path` are used instead. How a binary is fetched from the Loca
    /// CDN, whose URLs are not HuggingFace specs.
    pub url: Option<String>,
    pub rel_path: Option<String>,
    pub filename: Option<String>,
}
