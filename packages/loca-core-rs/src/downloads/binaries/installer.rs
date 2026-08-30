//! Installing a binary: resolve it, download it to staging, verify it, extract
//! it, and record what landed.
//!
//! `variant` is what is ON DISK; `target` is what the install AIMED for. They
//! differ only when the ideal variant was not resolvable and the install
//! degraded, and presence on the wire is judged by `target == desired_variant`,
//! so an unavoidable fallback settles rather than re-offering itself forever.
//! This core installs one variant (`cpu`), so the two are always equal, but
//! writing only one would make every install look degraded to a client.
//!
//! [`BinariesInstaller::failure`] keeps a failed install visible, so the
//! requirements snapshot can mark a missing binary retryable and say why.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::config::Channel;
use crate::contract::api::{
    BinaryKind, BinaryProbeResult, BinaryStatus, BinaryUpdateCheck, BinaryVariant,
    DownloadEntryDestination, DownloadStatus,
};
use crate::downloads::binaries::extract::extract_zip;
use crate::downloads::binaries::manifest::{load_manifest, BinaryManifest};
use crate::downloads::binaries::upstream::{resolve_entry, ResolvedBinary};
use crate::downloads::cancel::CancellationToken;
use crate::downloads::net::{Net, Redirect};
use crate::downloads::queue::DownloadQueue;
use crate::downloads::EnqueueSpec;
use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;
use crate::storage::deps::RequirementsNotifier;

/// The kinds this core can actually install. `BinaryKind` stays 3-valued on the
/// wire, but the sidecar kinds are in-process now and nothing here can execute
/// them, so offering them would download gigabytes for no launcher.
pub const INSTALLABLE_KINDS: &[BinaryKind] = &[BinaryKind::Deno];

/// What `versions.json` records for one installed binary.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct InstalledVersion {
    pub version: String,
    /// What is on disk.
    pub variant: Option<BinaryVariant>,
    /// What the install aimed for.
    pub target: Option<BinaryVariant>,
}

type Installed = BTreeMap<String, InstalledVersion>;

/// Downloads, verifies, extracts and records the binaries this core needs.
#[derive(Clone)]
pub struct BinariesInstaller {
    inner: Arc<Inner>,
}

struct Inner {
    net: Arc<Net>,
    queue: DownloadQueue,
    paths: Paths,
    channel: Channel,
    manifest_url: String,
    /// The last install failure per kind, cleared when a fresh attempt starts or
    /// succeeds. Read by the requirements snapshot.
    failures: Mutex<BTreeMap<String, String>>,
    /// Told whenever an install SETTLES, so the requirements snapshot is
    /// recomputed off the install lifecycle rather than the download queue.
    /// `None` until boot fills it.
    requirements: Mutex<Option<Arc<dyn RequirementsNotifier>>>,
}

impl BinariesInstaller {
    pub fn new(
        net: Arc<Net>,
        queue: DownloadQueue,
        paths: Paths,
        channel: Channel,
        manifest_url: &str,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                net,
                queue,
                paths,
                channel,
                manifest_url: manifest_url.to_string(),
                failures: Mutex::new(BTreeMap::new()),
                requirements: Mutex::new(None),
            }),
        }
    }

    /// Where an install SETTLING is reported, so the requirements snapshot can
    /// be recomputed and rebroadcast.
    ///
    /// The only signal that can clear a binary out of `missing`. A binaries row
    /// reaches `Completed` when its ARCHIVE lands, before [`Self::finish`]
    /// extracts and records it, so a recompute driven off the queue answers
    /// "still missing" and then never runs again: the queue is idle, so there
    /// is no further edge, and the client's gate never opens.
    pub fn attach_requirements(&self, requirements: Arc<dyn RequirementsNotifier>) {
        *lock(&self.inner.requirements) = Some(requirements);
    }

    /// Whether a requirements notifier was attached. A read port for the
    /// boot-seam suite, reading the LIVE field rather than boot's intent.
    pub fn has_requirements(&self) -> bool {
        lock(&self.inner.requirements).is_some()
    }

    /// Fire-and-forget, and a no-op on a core that wired no notifier.
    fn notify_requirements(&self) {
        let notifier = lock(&self.inner.requirements).clone();
        if let Some(notifier) = notifier {
            notifier.changed();
        }
    }

    /// Why the last install attempt for `kind` failed, or `None` when the most
    /// recent attempt is pending or succeeded.
    pub fn failure(&self, kind: BinaryKind) -> Option<String> {
        lock(&self.inner.failures).get(&kind.to_string()).cloned()
    }

    fn record_failure(&self, kind: BinaryKind, message: &str) {
        // Logged AND recorded: the log is for stderr, the map is what the
        // requirements snapshot reads to say why the binary is still missing.
        tracing::error!(target: "loca::downloads", "install {kind}: {message}");
        lock(&self.inner.failures).insert(kind.to_string(), message.to_string());
        // A failure settles the install too, and only a recompute puts the
        // reason in front of the user.
        self.notify_requirements();
    }

    fn clear_failure(&self, kind: BinaryKind) {
        lock(&self.inner.failures).remove(&kind.to_string());
    }

    /// The install state of every kind on the contract.
    ///
    /// **Never reaches the network.** It reads the cached manifest and the
    /// disk through `cached_manifest`, which returns `None` for every way a
    /// cache can be unusable rather than falling through to a fetch: this route
    /// is rendered by a client on open, not by a user asking for anything.
    pub async fn list(&self) -> Result<Vec<BinaryStatus>> {
        let manifest = self.cached_manifest().await;
        let installed = self.read_installed().await;
        let mut out = Vec::new();
        for kind in [BinaryKind::LlamaServer, BinaryKind::Deno] {
            let key = kind.to_string();
            let path = self.inner.paths.bin_path(&binary_name(kind));
            let on_disk = tokio::fs::metadata(&path).await.is_ok();
            let record = installed.get(&key);
            let recorded = record
                .map(|r| r.version.as_str())
                .filter(|version| !version.is_empty());
            // A pinned entry knows its published version; a resolver entry
            // does not until it asks GitHub, so it reports the installed one or
            // the literal "latest".
            let version = match manifest
                .as_ref()
                .and_then(|manifest| manifest.binaries.get(&key))
            {
                Some(entry) => entry.version_without_network(recorded),
                None => recorded.unwrap_or("unknown").to_string(),
            };
            out.push(BinaryStatus {
                kind,
                version,
                installed: on_disk,
                path: on_disk.then(|| path.display().to_string()),
                // Both only when installed, and both from the record.
                variant: on_disk.then(|| record.and_then(|r| r.variant)).flatten(),
                target: on_disk.then(|| record.and_then(|r| r.target)).flatten(),
                desired_variant: INSTALLABLE_KINDS
                    .contains(&kind)
                    .then_some(BinaryVariant::Cpu),
            });
        }
        Ok(out)
    }

    /// Install the named kinds, or every missing one when `kinds` is absent.
    ///
    /// **Absent and empty are different.** `None` means "install everything
    /// missing"; `Some(vec![])` means "install nothing" and returns no job ids.
    ///
    /// One kind failing to resolve must not block the others, so a failure is
    /// recorded and the loop continues. `update` (a single kind) surfaces its
    /// error to the caller instead.
    pub async fn install(&self, kinds: Option<Vec<BinaryKind>>) -> Result<Vec<String>> {
        let manifest = self.load(false).await?;
        let targets = match kinds {
            Some(explicit) => explicit,
            None => self.missing_kinds().await,
        };
        let mut job_ids = Vec::new();
        for kind in targets {
            match self.kickoff(kind, &manifest).await {
                Ok(job_id) => job_ids.push(job_id),
                Err(err) => self.record_failure(kind, &err.message),
            }
        }
        Ok(job_ids)
    }

    /// Force-update one binary to the manifest's published version.
    pub async fn update(&self, kind: BinaryKind) -> Result<String> {
        let manifest = self.load(true).await?;
        self.kickoff(kind, &manifest).await
    }

    /// Which installable kinds are not on disk.
    pub async fn missing_kinds(&self) -> Vec<BinaryKind> {
        let mut missing = Vec::new();
        for kind in INSTALLABLE_KINDS {
            let path = self.inner.paths.bin_path(&binary_name(*kind));
            if tokio::fs::metadata(&path).await.is_err() {
                missing.push(*kind);
            }
        }
        missing
    }

    /// Whether a newer version is published for each installable kind.
    ///
    /// **One kind's resolve failing does not fail the route.** Unauthenticated
    /// GitHub is rate-limited to about 60 requests an hour per IP, and offline
    /// does the same, so every kind emits a row either way, with
    /// `latest_version: "unknown"` and `available: false` when unresolved.
    pub async fn check(&self, cancel: &CancellationToken) -> Result<Vec<BinaryUpdateCheck>> {
        let manifest = self.load(true).await?;
        let installed = self.read_installed().await;
        let triple = crate::config::host_triple();
        let mut out = Vec::new();
        for kind in INSTALLABLE_KINDS {
            let key = kind.to_string();
            let installed_version = installed.get(&key).map(|r| r.version.clone());
            let on_disk = tokio::fs::metadata(self.inner.paths.bin_path(&binary_name(*kind)))
                .await
                .is_ok();
            let resolved = match manifest.binaries.get(&key) {
                None => None,
                Some(entry) => match resolve_entry(&self.inner.net, entry, triple, cancel).await {
                    Ok(resolved) => resolved,
                    Err(err) => {
                        tracing::warn!(
                            target: "loca::downloads",
                            "check {kind}: resolve failed: {}", err.message
                        );
                        None
                    }
                },
            };
            let latest_version = resolved
                .as_ref()
                .map(|r| r.version.clone())
                .unwrap_or_else(|| UNKNOWN_VERSION.to_string());
            out.push(BinaryUpdateCheck {
                kind: *kind,
                installed: on_disk,
                // Guarded on having resolved something, so an unreachable kind
                // never shows a phantom update to a version named "unknown".
                available: resolved.is_some()
                    && on_disk
                    && installed_version.as_deref() != Some(latest_version.as_str()),
                installed_version,
                latest_version,
            });
        }
        Ok(out)
    }

    /// The download size of each named kind, for the confirmation modal.
    ///
    /// Degrades per kind like [`Self::check`], and emits a row for every kind
    /// ASKED FOR rather than every kind it could resolve: the confirmation
    /// modal lists what the user is accepting, so a kind missing from the
    /// answer is a download nothing consented to.
    pub async fn probe(
        &self,
        kinds: &[BinaryKind],
        cancel: &CancellationToken,
    ) -> Result<Vec<BinaryProbeResult>> {
        let manifest = self.load(false).await?;
        let triple = crate::config::host_triple();
        let mut out = Vec::new();
        for kind in kinds {
            let unknown = BinaryProbeResult {
                kind: *kind,
                size_bytes: None,
                version: UNKNOWN_VERSION.to_string(),
            };
            let Some(entry) = manifest.binaries.get(&kind.to_string()) else {
                out.push(unknown);
                continue;
            };
            let resolved = match resolve_entry(&self.inner.net, entry, triple, cancel).await {
                Ok(Some(resolved)) => resolved,
                Ok(None) => {
                    out.push(unknown);
                    continue;
                }
                Err(err) => {
                    tracing::warn!(
                        target: "loca::downloads",
                        "probe {kind}: resolve failed: {}", err.message
                    );
                    out.push(unknown);
                    continue;
                }
            };
            // A pinned entry carries no size, so the modal falls back to a
            // HEAD; a resolver entry learned it from the release asset.
            let size_bytes = match resolved.size_bytes {
                Some(size) => Some(size),
                None => self
                    .inner
                    .net
                    .head(&resolved.url, Redirect::Follow, cancel)
                    .await
                    .ok()
                    .and_then(|head| head.header_u64("content-length")),
            };
            out.push(BinaryProbeResult {
                kind: *kind,
                size_bytes: size_bytes.map(|n| n as i64),
                version: resolved.version,
            });
        }
        Ok(out)
    }

    /// Finish binary installs a process exit interrupted before their archive
    /// was extracted.
    ///
    /// The usual cause is the in-app update flow tearing this process down
    /// mid-download. Resuming the BYTES is not enough: the extract-and-record
    /// half lived in the task [`Self::kickoff`] spawned, which died with the
    /// old process. So [`DownloadQueue::resume_pending`] skips `binaries` rows
    /// and this re-runs the whole kickoff, or the row sits `Pending` forever
    /// with the archive orphaned and the requirements gate unsatisfiable.
    ///
    /// Best-effort: a per-kind failure never blocks boot. Cache-first, so a
    /// boot with no interrupted work makes no request at all.
    pub async fn reconcile_interrupted_installs(&self) {
        let stale: Vec<_> = match self.inner.queue.snapshot().await {
            Ok(rows) => rows
                .into_iter()
                .filter(|row| {
                    row.destination == DownloadEntryDestination::Binaries
                        && row.status == DownloadStatus::Pending
                })
                .collect(),
            Err(err) => {
                tracing::warn!(target: "loca::downloads", "reconcile: {err}");
                return;
            }
        };
        if stale.is_empty() {
            return;
        }

        let mut kinds: Vec<BinaryKind> = Vec::new();
        for row in &stale {
            if let Some(kind) = kind_from_group_id(&row.group_id) {
                if !kinds.contains(&kind) {
                    kinds.push(kind);
                }
            }
            // Drop the orphaned row and its partial: kickoff mints a fresh id,
            // so leaving this one strands the status bar on "Downloading" with
            // no worker. Nothing here is in flight to be no-opped on.
            if let Err(err) = self.inner.queue.remove(&row.id).await {
                tracing::warn!(target: "loca::downloads", "reconcile: dropping {}: {err}", row.id);
            }
        }
        if kinds.is_empty() {
            return;
        }

        // Cache-first: the manifest was cached during the original consented
        // update, so re-initiating uses it with no new network.
        let manifest = match self.load(false).await {
            Ok(manifest) => manifest,
            Err(err) => {
                tracing::warn!(
                    target: "loca::downloads",
                    "reconcile: binary manifest unavailable ({}); leaving interrupted installs for retry",
                    err.message
                );
                return;
            }
        };
        for kind in kinds {
            match self.kickoff(kind, &manifest).await {
                Ok(_) => tracing::info!(
                    target: "loca::downloads",
                    "reconcile: re-initiated interrupted install of {kind}"
                ),
                Err(err) => self.record_failure(kind, &err.message),
            }
        }
    }

    /// The manifest, for `GET /api/v1/binaries/manifest`.
    pub async fn manifest(&self) -> Result<BinaryManifest> {
        self.load(false).await
    }

    // --- internals ---------------------------------------------------------

    /// Resolve, download to staging, verify, extract, and record.
    async fn kickoff(&self, kind: BinaryKind, manifest: &BinaryManifest) -> Result<String> {
        // A fresh attempt clears the previous reason, so a running retry does
        // not report the old failure as current.
        self.clear_failure(kind);
        if !INSTALLABLE_KINDS.contains(&kind) {
            return Err(AppError::new(
                ErrorCode::ValidationError,
                format!(
                    "{kind} is on the wire contract but this core installs only \
                     {INSTALLABLE_KINDS:?}; its sidecar was absorbed and nothing here can run it"
                ),
            ));
        }
        let entry = manifest.binaries.get(&kind.to_string()).ok_or_else(|| {
            AppError::new(
                ErrorCode::ManifestFetchFailed,
                format!("the binary manifest has no entry for {kind}"),
            )
        })?;
        let cancel = CancellationToken::new();
        let resolved = resolve_entry(
            &self.inner.net,
            entry,
            crate::config::host_triple(),
            &cancel,
        )
        .await?
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::ProviderError,
                format!("no installable {kind} asset is published yet"),
            )
        })?;

        let rel_path = format!("staging/{}.zip", kind);
        let spec = EnqueueSpec {
            source: format!("@upstream/{kind}/{}/{}", resolved.version, kind),
            destination: DownloadEntryDestination::Binaries,
            group_id: group_id_for(kind),
            size_hint: resolved.size_bytes,
            // Pinned from the release digest, so the archive is verified before
            // anything is extracted out of it.
            sha256: Some(resolved.sha256.clone()),
            url: Some(resolved.url.clone()),
            rel_path: Some(rel_path),
            filename: Some(format!("{kind}.zip")),
        };
        let job_id = self.inner.queue.id_for(&spec)?;

        let installer = self.clone();
        let resolved_for_task = resolved.clone();
        tokio::spawn(async move {
            if let Err(err) = installer.finish(kind, &spec, &resolved_for_task).await {
                installer.record_failure(kind, &err.message);
            }
        });
        Ok(job_id)
    }

    /// The half that runs after the archive lands: extract and record.
    ///
    /// Separate from the download because the queue owns bytes and this owns
    /// what they become, and why a `binaries` row is never generically resumed:
    /// this step lives in the process that started it.
    async fn finish(
        &self,
        kind: BinaryKind,
        spec: &EnqueueSpec,
        resolved: &ResolvedBinary,
    ) -> Result<()> {
        let archive = self.inner.queue.enqueue(spec).await?;
        let bin_dir = self.inner.paths.bin_dir();
        let exe = binary_name(kind);
        let archive_path = archive.clone();
        let bin_dir_for_task = bin_dir.clone();
        let exe_for_task = exe.clone();
        // Extraction is synchronous and CPU-bound, so it runs off the async
        // workers rather than stalling one for the length of a decompress.
        tokio::task::spawn_blocking(move || {
            extract_zip(&archive_path, &bin_dir_for_task, &exe_for_task)
        })
        .await
        .map_err(|err| {
            AppError::new(
                ErrorCode::ExtractFailed,
                format!("extraction task for {kind} failed: {err}"),
            )
        })??;
        // Leaving it would keep a full copy of every installed binary forever.
        let _ = tokio::fs::remove_file(&archive).await;
        self.write_installed_version(kind, &resolved.version)
            .await?;
        self.clear_failure(kind);
        // After the version is recorded, never before: the first moment
        // `list()` reports the binary as installed.
        self.notify_requirements();
        Ok(())
    }

    async fn write_installed_version(&self, kind: BinaryKind, version: &str) -> Result<()> {
        let mut installed = self.read_installed().await;
        installed.insert(
            kind.to_string(),
            InstalledVersion {
                version: version.to_string(),
                // BOTH, always: a client judges presence by `target`, so
                // recording only `variant` makes every install look degraded.
                variant: Some(BinaryVariant::Cpu),
                target: Some(BinaryVariant::Cpu),
            },
        );
        let path = self.versions_path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let text = serde_json::to_string_pretty(&installed).map_err(|err| {
            AppError::new(
                ErrorCode::InternalError,
                format!("could not encode versions.json: {err}"),
            )
        })?;
        tokio::fs::write(&path, text).await?;
        Ok(())
    }

    async fn read_installed(&self) -> Installed {
        let Ok(text) = tokio::fs::read_to_string(self.versions_path()).await else {
            return Installed::new();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    fn versions_path(&self) -> PathBuf {
        self.inner.paths.bin_dir().join("versions.json")
    }

    async fn load(&self, force: bool) -> Result<BinaryManifest> {
        load_manifest(
            &self.inner.net,
            self.inner.channel,
            &self.inner.paths.cache_dir(),
            &self.inner.manifest_url,
            force,
            &CancellationToken::new(),
        )
        .await
    }

    /// The manifest with no chance of a fetch, for `list`.
    ///
    /// Deliberately not `load(false)`, which short-circuits only on a GOOD
    /// cache: a cache read returns `None` when absent, unreadable, malformed or
    /// unverifiable, and the other three would fall through to a fetch.
    async fn cached_manifest(&self) -> Option<BinaryManifest> {
        if self.inner.channel == Channel::Dev {
            return Some(crate::downloads::binaries::manifest::dev_manifest());
        }
        crate::downloads::binaries::manifest::cached_manifest(&self.inner.paths.cache_dir()).await
    }
}

/// The `group_id` every row a single kind's install creates is tagged with.
///
/// The one thing tying an orphaned row back to the kind that started it, read
/// on boot by [`BinariesInstaller::reconcile_interrupted_installs`]. Minted and
/// parsed in one place so the two cannot drift.
pub fn group_id_for(kind: BinaryKind) -> String {
    format!("{GROUP_ID_PREFIX}{kind}")
}

fn kind_from_group_id(group_id: &str) -> Option<BinaryKind> {
    match group_id.strip_prefix(GROUP_ID_PREFIX)? {
        "deno" => Some(BinaryKind::Deno),
        "llama-server" => Some(BinaryKind::LlamaServer),
        _ => None,
    }
}

const GROUP_ID_PREFIX: &str = "binaries:";

/// What a version reads as when it could not be resolved, which is what lets a
/// degraded row still be a row.
const UNKNOWN_VERSION: &str = "unknown";

/// The storage screen's view of the helper binaries.
///
/// Implemented here rather than in `storage/`: the fact lives with the package
/// that owns it, and the storage package holds only the question.
#[async_trait::async_trait]
impl crate::storage::deps::BinariesSource for BinariesInstaller {
    /// Only what is actually on disk, so the screen never offers to reclaim a
    /// row that would free nothing.
    async fn installed(&self) -> Result<Vec<crate::storage::deps::InstalledBinary>> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|status| status.installed)
            .filter_map(|status| {
                status
                    .path
                    .map(|path| crate::storage::deps::InstalledBinary {
                        kind: status.kind.to_string(),
                        path: PathBuf::from(path),
                    })
            })
            .collect())
    }

    /// llama.cpp and sherpa are in-process, so no live subsystem is a spawned
    /// binary, and `deno` is required whenever it is installable.
    async fn locked_kinds(&self) -> Result<std::collections::HashSet<String>> {
        Ok(crate::requirements::required_binary_kinds()
            .iter()
            .map(std::string::ToString::to_string)
            .collect())
    }
}

/// The on-disk name of a binary. Not channel-suffixed: downloaded binaries are
/// isolated by the per-channel `bin` directory and keep their original names.
pub fn binary_name(kind: BinaryKind) -> String {
    if cfg!(windows) {
        format!("{kind}.exe")
    } else {
        kind.to_string()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use serde_json::Value;

    use super::*;
    use crate::db::migrate::migrate;
    use crate::db::Db;
    use crate::downloads::binaries::manifest::test_trust::Trusted;
    use crate::downloads::binaries::upstream::test_github::GithubHost;
    use crate::downloads::deps::NoCatalog;
    use crate::downloads::store::DownloadStore;
    use crate::downloads::testing::{
        published_latest_manifest, published_stable_manifest, Serve, Upstream,
    };
    use crate::downloads::transfer::Transfer;

    const HF: &str = "https://huggingface.co";

    fn installer(dir: &tempfile::TempDir) -> BinariesInstaller {
        installer_on(dir, Channel::Dev)
    }

    fn installer_on(dir: &tempfile::TempDir, channel: Channel) -> BinariesInstaller {
        installer_with(dir, channel, "http://127.0.0.1:1/binaries.json").0
    }

    /// The installer plus the queue behind it, for the tests that stage rows.
    fn installer_with(
        dir: &tempfile::TempDir,
        channel: Channel,
        manifest_url: &str,
    ) -> (BinariesInstaller, DownloadQueue) {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| migrate(conn)).unwrap();
        let store = DownloadStore::new(db, HF);
        let net = Arc::new(Net::new().unwrap());
        let transfer = Arc::new(
            Transfer::new(Arc::clone(&net), Arc::new(NoCatalog), HF)
                .with_intervals(Duration::from_secs(5), Duration::from_millis(1)),
        );
        let paths = Paths::rooted(dir.path());
        let queue = DownloadQueue::new(store, transfer, paths.clone(), HF)
            .with_delay(Duration::from_millis(0));
        let installer = BinariesInstaller::new(net, queue.clone(), paths, channel, manifest_url);
        (installer, queue)
    }

    /// Write `document`, signed by the trust root the test swapped in, where
    /// the production cache reader will find it.
    async fn stage_cache(installer: &BinariesInstaller, trust: &Trusted, document: Value) {
        let cache_dir = installer.inner.paths.cache_dir();
        tokio::fs::create_dir_all(&cache_dir).await.unwrap();
        tokio::fs::write(
            crate::downloads::binaries::manifest::cache_path(&cache_dir),
            trust.sign(document),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_empty_kinds_array_installs_nothing() {
        // Absent means "everything missing", empty means "nothing".
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);

        let jobs = installer.install(Some(Vec::new())).await.unwrap();

        assert!(jobs.is_empty(), "an explicit empty list started a download");
        assert!(
            installer.failure(BinaryKind::Deno).is_none(),
            "nothing was attempted, so nothing can have failed"
        );
    }

    #[tokio::test]
    async fn an_absent_kinds_list_targets_everything_missing() {
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);

        assert_eq!(installer.missing_kinds().await, vec![BinaryKind::Deno]);

        // With the binary on disk, nothing is missing.
        place_deno(&dir);
        assert!(installer.missing_kinds().await.is_empty());
    }

    #[tokio::test]
    async fn a_failed_install_is_visible_to_the_requirements_snapshot() {
        // Swallowing it leaves a binary simply absent, with no reason given.
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);

        // A kind this core cannot install: resolvable question, definite answer.
        let jobs = installer
            .install(Some(vec![BinaryKind::LlamaServer]))
            .await
            .unwrap();

        assert!(jobs.is_empty());
        let failure = installer
            .failure(BinaryKind::LlamaServer)
            .expect("the failure was swallowed rather than recorded");
        assert!(failure.contains("llama-server"), "{failure}");
    }

    /// Records what the on-disk install record said at the MOMENT it was told,
    /// the only way to prove the notification is not pre-extract.
    struct RecordingNotifier {
        versions: PathBuf,
        seen: Mutex<Vec<bool>>,
        tx: tokio::sync::mpsc::UnboundedSender<()>,
    }

    impl RequirementsNotifier for RecordingNotifier {
        fn changed(&self) {
            let recorded =
                std::fs::read_to_string(&self.versions).is_ok_and(|text| text.contains("\"deno\""));
            lock(&self.seen).push(recorded);
            let _ = self.tx.send(());
        }
    }

    fn recording(
        installer: &BinariesInstaller,
    ) -> (
        Arc<RecordingNotifier>,
        tokio::sync::mpsc::UnboundedReceiver<()>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let notifier = Arc::new(RecordingNotifier {
            versions: installer.versions_path(),
            seen: Mutex::new(Vec::new()),
            tx,
        });
        installer.attach_requirements(Arc::clone(&notifier) as Arc<dyn RequirementsNotifier>);
        (notifier, rx)
    }

    /// Put a `deno` binary on disk under `dir`'s `bin`, so it reads as installed.
    fn place_deno(dir: &tempfile::TempDir) {
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(binary_name(BinaryKind::Deno)), b"x").unwrap();
    }

    /// The `deno` row out of a `list` answer, which every kind on the contract
    /// carries.
    fn deno_of(listed: &[BinaryStatus]) -> &BinaryStatus {
        listed
            .iter()
            .find(|s| s.kind == BinaryKind::Deno)
            .expect("deno is on the contract")
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut buf);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, contents) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
        buf.into_inner()
    }

    use crate::files::sha256_hex;

    #[tokio::test]
    async fn a_finished_install_tells_requirements_once_the_binary_reads_as_installed() {
        // A binaries row hits `Completed` when its ARCHIVE lands, so a client
        // recomputing off the queue is told "still missing" and never asked
        // again. Only this notification, after the record, clears it.
        let exe = binary_name(BinaryKind::Deno);
        let archive = zip_bytes(&[(exe.as_str(), b"EXE-BYTES")]);
        let asset = Upstream::start(Serve::Ranged(archive.clone())).await;
        let release = serde_json::json!({
            "tag_name": "v2.8.2",
            "assets": [{
                "name": format!("deno-{}.zip", crate::config::host_triple()),
                "browser_download_url": asset.url("/deno.zip"),
                "state": "uploaded",
                "digest": format!("sha256:{}", sha256_hex(&archive)),
                "size": archive.len(),
            }],
        });
        let github = Upstream::start(Serve::Json(release.to_string())).await;
        let _guard = GithubHost::set(github.base());
        let trust = Trusted::new();
        let dir = tempfile::tempdir().unwrap();
        let (installer, _queue) =
            installer_with(&dir, Channel::Latest, "http://127.0.0.1:1/binaries.json");
        stage_cache(&installer, &trust, published_latest_manifest()).await;
        let (notifier, mut told) = recording(&installer);

        installer
            .install(Some(vec![BinaryKind::Deno]))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), told.recv())
            .await
            .expect("the install settled without telling requirements anything")
            .expect("the notifier was dropped");

        assert_eq!(
            lock(&notifier.seen).as_slice(),
            [true],
            "requirements was told before the installed version was recorded, so the \
             recompute it triggers answers \"still missing\""
        );
        let deno = installer
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.kind == BinaryKind::Deno)
            .expect("deno is on the contract");
        assert!(deno.installed);
    }

    #[tokio::test]
    async fn a_failed_install_tells_requirements_too() {
        // A failure settles the install as much as a success does, and only a
        // recompute puts the reason in front of the user.
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);
        let (notifier, mut told) = recording(&installer);

        installer
            .install(Some(vec![BinaryKind::LlamaServer]))
            .await
            .unwrap();

        told.recv().await.expect("the notifier was dropped");
        assert_eq!(lock(&notifier.seen).len(), 1);
    }

    #[tokio::test]
    async fn one_kind_failing_does_not_block_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);
        installer
            .install(Some(vec![BinaryKind::LlamaServer, BinaryKind::Deno]))
            .await
            .unwrap();
        // The uninstallable kind records its failure without aborting the batch.
        assert!(installer.failure(BinaryKind::LlamaServer).is_some());
        assert!(installer.failure(BinaryKind::Deno).is_none());
    }

    #[tokio::test]
    async fn list_reports_the_variant_and_the_target_it_aimed_for() {
        // A client judges presence by `target == desiredVariant`, so recording
        // only `variant` reads as perpetually missing.
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);
        place_deno(&dir);
        installer
            .write_installed_version(BinaryKind::Deno, "v2.8.2")
            .await
            .unwrap();

        let listed = installer.list().await.unwrap();
        let deno = deno_of(&listed);

        assert!(deno.installed);
        assert_eq!(deno.version, "v2.8.2");
        assert_eq!(deno.variant, Some(BinaryVariant::Cpu));
        assert_eq!(
            deno.target,
            Some(BinaryVariant::Cpu),
            "the install recorded what it aimed for as well as what landed"
        );
        assert_eq!(deno.desired_variant, Some(BinaryVariant::Cpu));
    }

    #[tokio::test]
    async fn list_reports_a_kind_that_is_not_installed_without_a_path_or_variant() {
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);

        let listed = installer.list().await.unwrap();
        let deno = deno_of(&listed);

        assert!(!deno.installed);
        assert_eq!(deno.path, None);
        assert_eq!(deno.variant, None);
        assert_eq!(deno.target, None);
    }

    #[tokio::test]
    async fn list_covers_every_kind_on_the_contract_even_the_uninstallable_ones() {
        // A client renders a row per kind on the wire, so both are listed.
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);

        let listed = installer.list().await.unwrap();

        assert_eq!(listed.len(), 2);
        let uninstallable = listed
            .iter()
            .find(|s| s.kind == BinaryKind::LlamaServer)
            .unwrap();
        assert!(!uninstallable.installed);
        assert_eq!(
            uninstallable.desired_variant, None,
            "a kind this core cannot install has no variant it desires"
        );
    }

    #[tokio::test]
    async fn list_makes_no_outbound_call() {
        // The consent rule: a plain list reads the cache and the disk.
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);

        installer.list().await.unwrap();

        assert_eq!(installer.inner.net.calls(), 0);
    }

    #[tokio::test]
    async fn list_makes_no_outbound_call_on_any_shipping_channel_or_cache_state() {
        // The test above runs on `Channel::Dev`, which returns an in-code
        // manifest before the cache is consulted, so it cannot reach the branch
        // it names. On a shipping channel a cache read returns `None` when
        // absent, unreadable, malformed or unverifiable, and a guard on the
        // file merely EXISTING lets three of those four reach the internet.
        for channel in [Channel::Stable, Channel::Latest] {
            for (what, cache) in
                [
                    ("absent", None),
                    ("truncated", Some("{\"schemaVersion\":1,".to_string())),
                    ("not json at all", Some("<html>404</html>".to_string())),
                    (
                        "well-formed but unsigned",
                        Some(
                            serde_json::to_string(
                                &crate::downloads::binaries::manifest::dev_manifest(),
                            )
                            .unwrap(),
                        ),
                    ),
                ]
            {
                let dir = tempfile::tempdir().unwrap();
                let installer = installer_on(&dir, channel);
                if let Some(cache) = cache {
                    let cache_dir = installer.inner.paths.cache_dir();
                    std::fs::create_dir_all(&cache_dir).unwrap();
                    std::fs::write(
                        crate::downloads::binaries::manifest::cache_path(&cache_dir),
                        cache,
                    )
                    .unwrap();
                }

                let listed = installer.list().await.unwrap();

                assert_eq!(listed.len(), 2, "{channel} / {what}");
                assert_eq!(
                    installer.inner.net.calls(),
                    0,
                    "a plain list reached the network on {channel} with a {what} cache"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_pinned_manifest_reports_its_published_version_without_asking_upstream() {
        // A stable entry knows its version, so reporting "unknown" hides what
        // the client needs to decide whether an install is offered.
        let dir = tempfile::tempdir().unwrap();
        let trust = Trusted::new();
        let installer = installer_on(&dir, Channel::Stable);
        stage_cache(&installer, &trust, published_stable_manifest()).await;

        let listed = installer.list().await.unwrap();
        let deno = deno_of(&listed);

        assert_eq!(deno.version, "v2.8.2");
        assert_eq!(installer.inner.net.calls(), 0);
    }

    #[tokio::test]
    async fn a_resolver_manifest_reports_latest_until_something_is_installed() {
        // A resolver entry does not know its version without asking GitHub,
        // and asking is what the consent rule forbids.
        let dir = tempfile::tempdir().unwrap();
        let trust = Trusted::new();
        let installer = installer_on(&dir, Channel::Latest);
        stage_cache(&installer, &trust, published_latest_manifest()).await;

        let listed = installer.list().await.unwrap();
        assert_eq!(deno_of(&listed).version, "latest");

        installer
            .write_installed_version(BinaryKind::Deno, "v2.8.1")
            .await
            .unwrap();
        let listed = installer.list().await.unwrap();
        assert_eq!(
            deno_of(&listed).version,
            "v2.8.1",
            "an installed resolver entry reports what is on disk"
        );
        assert_eq!(installer.inner.net.calls(), 0);
    }

    // -- check and probe degrade per kind -----------------------------------

    #[tokio::test]
    async fn check_still_answers_when_upstream_cannot_be_reached() {
        // Unauthenticated GitHub is rate-limited to about 60 requests an hour
        // per IP, so an error envelope here turns a normal day into a broken
        // "check for updates" button. Offline does the same.
        let trust = Trusted::new();
        // Port 1 never listens, so the release lookup fails at connect.
        let _github = GithubHost::set("http://127.0.0.1:1");
        let manifest = Upstream::start(Serve::Json(trust.sign(published_latest_manifest()))).await;
        let dir = tempfile::tempdir().unwrap();
        let (installer, _queue) =
            installer_with(&dir, Channel::Latest, &manifest.url("/binaries.json"));
        // INSTALLED, with a recorded version: a kind not on disk is not
        // "available" under any implementation, so the phantom-update guard
        // would be untested on a fixture that omits it.
        place_deno(&dir);
        installer
            .write_installed_version(BinaryKind::Deno, "v2.8.2")
            .await
            .unwrap();

        let checked = installer.check(&CancellationToken::new()).await.unwrap();

        let deno = checked
            .iter()
            .find(|row| row.kind == BinaryKind::Deno)
            .expect("a kind that could not be resolved still gets a row");
        assert_eq!(deno.latest_version, "unknown");
        assert!(deno.installed);
        assert_eq!(deno.installed_version.as_deref(), Some("v2.8.2"));
        assert!(
            !deno.available,
            "an unresolvable kind advertised an update to a version called \"unknown\""
        );
    }

    #[tokio::test]
    async fn probe_answers_for_every_kind_it_was_asked_about() {
        // The confirmation modal lists what the user is accepting, so a kind
        // missing from the answer is a download nothing consented to.
        let trust = Trusted::new();
        let _github = GithubHost::set("http://127.0.0.1:1");
        let dir = tempfile::tempdir().unwrap();
        let installer = installer_on(&dir, Channel::Latest);
        stage_cache(&installer, &trust, published_latest_manifest()).await;

        let probed = installer
            .probe(
                &[BinaryKind::Deno, BinaryKind::LlamaServer],
                &CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(probed.len(), 2, "a kind was dropped from the answer");
        for row in &probed {
            assert_eq!(row.version, "unknown", "{:?}", row.kind);
            assert_eq!(row.size_bytes, None, "{:?}", row.kind);
        }
    }

    #[tokio::test]
    async fn probe_answers_for_a_kind_the_manifest_does_not_carry_at_all() {
        let trust = Trusted::new();
        let dir = tempfile::tempdir().unwrap();
        let installer = installer_on(&dir, Channel::Latest);
        let mut document = published_latest_manifest();
        document["binaries"].as_object_mut().unwrap().remove("deno");
        stage_cache(&installer, &trust, document).await;

        let probed = installer
            .probe(&[BinaryKind::Deno], &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(probed.len(), 1);
        assert_eq!(probed[0].version, "unknown");
    }

    #[tokio::test]
    async fn probe_heads_for_a_size_a_pinned_entry_does_not_publish() {
        // A pinned entry carries no size, so the modal falls back to a HEAD;
        // without it every stable confirmation shows no size at all.
        let trust = Trusted::new();
        let artifact = Upstream::start(Serve::Status(200))
            .await
            .with_head_headers(&[("content-length", "40000000")]);
        let dir = tempfile::tempdir().unwrap();
        let installer = installer_on(&dir, Channel::Stable);
        let mut document = published_stable_manifest();
        document["binaries"]["deno"]["platforms"][crate::config::host_triple()]["url"] =
            Value::String(artifact.url("/deno.zip"));
        stage_cache(&installer, &trust, document).await;

        let probed = installer
            .probe(&[BinaryKind::Deno], &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(probed[0].version, "v2.8.2");
        assert_eq!(probed[0].size_bytes, Some(40_000_000));
    }

    // -- boot reconcile -----------------------------------------------------

    #[tokio::test]
    async fn reconcile_re_initiates_an_interrupted_binary_install() {
        // `resume_pending` skips `binaries` rows, which is only safe because
        // this finishes them: otherwise the row sits `Pending` forever with the
        // archive orphaned and the requirements gate unsatisfiable.
        let trust = Trusted::new();
        let release = serde_json::json!({
            "tag_name": "v2.8.2",
            "assets": [{
                "name": format!("deno-{}.zip", crate::config::host_triple()),
                "browser_download_url": "https://example.invalid/deno.zip",
                "state": "uploaded",
                "digest": "sha256:abc",
                "size": 4096,
            }],
        });
        let github = Upstream::start(Serve::Json(release.to_string())).await;
        let _guard = GithubHost::set(github.base());
        let dir = tempfile::tempdir().unwrap();
        let (installer, queue) =
            installer_with(&dir, Channel::Latest, "http://127.0.0.1:1/binaries.json");
        stage_cache(&installer, &trust, published_latest_manifest()).await;

        // The state a kill mid-install leaves: a Pending binaries row tagged
        // with the kind that started it, and no worker anywhere.
        let stale = EnqueueSpec {
            source: "@upstream/deno/v2.8.1/deno".into(),
            destination: DownloadEntryDestination::Binaries,
            group_id: group_id_for(BinaryKind::Deno),
            size_hint: None,
            sha256: None,
            url: Some("http://127.0.0.1:1/deno.zip".into()),
            rel_path: Some("staging/deno.zip".into()),
            filename: Some("deno.zip".into()),
        };
        let stale_id = queue.id_for(&stale).unwrap();
        queue
            .store_for_test()
            .upsert_pending(&stale_id, &stale, &dir.path().join("bin/staging/deno.zip"))
            .await
            .unwrap();

        installer.reconcile_interrupted_installs().await;

        assert!(
            queue
                .store_for_test()
                .get_row(&stale_id)
                .await
                .unwrap()
                .is_none(),
            "the orphaned row survived, so the status bar stays on Downloading forever"
        );
        assert_eq!(
            github.calls().len(),
            1,
            "the kind was re-resolved and re-kicked off"
        );
    }

    #[tokio::test]
    async fn reconcile_makes_no_request_when_nothing_was_interrupted() {
        // A boot with a settled queue must not turn into a request.
        let dir = tempfile::tempdir().unwrap();
        let installer = installer_on(&dir, Channel::Stable);

        installer.reconcile_interrupted_installs().await;

        assert_eq!(installer.inner.net.calls(), 0);
    }

    #[tokio::test]
    async fn reconcile_leaves_a_models_row_alone() {
        // The BINARIES half of boot: `start` owns the models rows, and
        // touching them here would double-resume them.
        let dir = tempfile::tempdir().unwrap();
        let (installer, queue) =
            installer_with(&dir, Channel::Stable, "http://127.0.0.1:1/binaries.json");
        let models = EnqueueSpec {
            source: "@u/r/main/model.gguf".into(),
            destination: DownloadEntryDestination::Models,
            group_id: "g".into(),
            size_hint: None,
            sha256: None,
            url: Some("http://127.0.0.1:1/f".into()),
            rel_path: None,
            filename: None,
        };
        let id = queue.id_for(&models).unwrap();
        queue
            .store_for_test()
            .upsert_pending(&id, &models, &dir.path().join("models/u/r/model.gguf"))
            .await
            .unwrap();

        installer.reconcile_interrupted_installs().await;

        assert!(queue.store_for_test().get_row(&id).await.unwrap().is_some());
        assert_eq!(installer.inner.net.calls(), 0);
    }

    #[test]
    fn a_group_id_round_trips_to_the_kind_that_minted_it() {
        // The only thing tying an orphaned row back to its kind, so minting
        // and parsing must be one pair rather than two string literals.
        for kind in [BinaryKind::Deno, BinaryKind::LlamaServer] {
            assert_eq!(kind_from_group_id(&group_id_for(kind)), Some(kind));
        }
        assert_eq!(kind_from_group_id("g"), None);
        assert_eq!(kind_from_group_id("binaries:something-else"), None);
    }

    #[tokio::test]
    async fn a_fresh_attempt_clears_the_previous_failure_reason() {
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);
        installer.record_failure(BinaryKind::Deno, "an old reason");

        installer.clear_failure(BinaryKind::Deno);

        assert!(installer.failure(BinaryKind::Deno).is_none());
    }

    #[tokio::test]
    async fn the_installed_record_round_trips_through_versions_json() {
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);

        installer
            .write_installed_version(BinaryKind::Deno, "v2.8.2")
            .await
            .unwrap();

        let read_back = installer.read_installed().await;
        assert_eq!(
            read_back.get("deno"),
            Some(&InstalledVersion {
                version: "v2.8.2".into(),
                variant: Some(BinaryVariant::Cpu),
                target: Some(BinaryVariant::Cpu),
            })
        );
    }

    #[tokio::test]
    async fn a_corrupt_versions_file_reads_as_empty_rather_than_failing_every_list() {
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("versions.json"), b"{not json").unwrap();

        assert!(installer.read_installed().await.is_empty());
        assert_eq!(installer.list().await.unwrap().len(), 2);
    }
}
