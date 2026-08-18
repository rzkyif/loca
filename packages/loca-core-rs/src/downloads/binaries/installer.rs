//! Installing a binary: resolve it, download it to staging, verify it, extract
//! it, and record what landed.
//!
//! # `variant` and `target` are different questions
//!
//! `variant` is what is ON DISK. `target` is what the install AIMED for. They
//! differ only when the ideal variant was not resolvable upstream and the
//! install degraded to a lesser one, and the distinction is load-bearing on the
//! wire: presence is judged by `target == desired_variant`, so an unavoidable
//! fallback settles instead of reading as perpetually missing and re-offering
//! itself in the requirements popup forever.
//!
//! This core installs one variant (`cpu`), so the two are always equal here.
//! Recording both anyway is not ceremony: the field is on the contract, the
//! client switches on it, and writing only one would make every install look
//! like a degraded one to a client that checks.
//!
//! # A failed install has to stay visible
//!
//! [`BinariesInstaller::failure`] is what lets the requirements snapshot mark a
//! missing binary as retryable and say why. Swallowing the error would leave the
//! user with a binary that is simply absent, no reason given, and a retry button
//! that reports nothing when it fails again.

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

/// The kinds this core can actually install.
///
/// `BinaryKind` stays 3-valued on the wire (`@loca/shared` is the contract and
/// does not narrow until cutover), but the sidecar kinds were absorbed by D14
/// and nothing here can execute them. Offering them would mean downloading
/// gigabytes for a binary with no launcher. A tracked difference, recorded like
/// D-039, never a placeholder.
pub const INSTALLABLE_KINDS: &[BinaryKind] = &[BinaryKind::Deno];

/// What `versions.json` records for one installed binary.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct InstalledVersion {
    pub version: String,
    /// What is on disk.
    pub variant: Option<BinaryVariant>,
    /// What the install aimed for. See the module docs.
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
            }),
        }
    }

    /// Why the last install attempt for `kind` failed, or `None` when the most
    /// recent attempt is pending or succeeded.
    pub fn failure(&self, kind: BinaryKind) -> Option<String> {
        lock(&self.inner.failures).get(&kind.to_string()).cloned()
    }

    fn record_failure(&self, kind: BinaryKind, message: &str) {
        // Logged AND recorded. The log is for whoever is reading stderr; the map
        // is what the requirements snapshot reads to tell the user why the
        // binary they asked for is still missing.
        tracing::error!(target: "loca::downloads", "install {kind}: {message}");
        lock(&self.inner.failures).insert(kind.to_string(), message.to_string());
    }

    fn clear_failure(&self, kind: BinaryKind) {
        lock(&self.inner.failures).remove(&kind.to_string());
    }

    /// The install state of every kind on the contract.
    ///
    /// **Never reaches the network.** It reads the cached manifest and the
    /// disk, and the read is `cached_manifest`, which returns `None` for every
    /// way a cache can be unusable rather than falling through to a fetch. This
    /// is a deliberate divergence from the oracle (`manager.ts:281`), which
    /// fetches when it has no cache; see the ADR. The repo's rule is absolute
    /// and this route is rendered by a client on open, not by a user asking for
    /// anything.
    pub async fn list(&self) -> Result<Vec<BinaryStatus>> {
        let manifest = self.cached_manifest().await;
        let installed = self.read_installed().await;
        let mut out = Vec::new();
        for kind in [
            BinaryKind::LlamaServer,
            BinaryKind::LocaCoreSpeech,
            BinaryKind::Deno,
        ] {
            let key = kind.to_string();
            let path = self.inner.paths.bin_path(&binary_name(kind));
            let on_disk = tokio::fs::metadata(&path).await.is_ok();
            let record = installed.get(&key);
            let recorded = record
                .map(|r| r.version.as_str())
                .filter(|version| !version.is_empty());
            // A pinned entry knows its published version; a resolver entry does
            // not until it asks GitHub, so it reports the installed one or the
            // literal "latest" (`manager.ts:293-297`). With no manifest at all
            // there is nothing to report but what is on disk.
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
                // Both only when installed, and both from the record: see the
                // module docs on why writing one and not the other is wrong.
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
    /// **Absent and empty are different** (D-028). `None` means "install
    /// everything missing"; `Some(vec![])` means "install nothing" and returns
    /// no job ids. Collapsing them would make a client that sends an explicit
    /// empty list trigger a full install.
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
    /// **One kind's resolve failing does not fail the route.** The oracle wraps
    /// each kind in try/catch, logs, and still pushes a row
    /// (`manager.ts:340-350`), and the reason is not tidiness: unauthenticated
    /// GitHub is rate-limited to about 60 requests an hour per IP, so a
    /// perfectly normal day turns the whole "check for updates" button into an
    /// error envelope where the oracle answers "nothing available". Offline does
    /// the same. Every kind therefore emits a row either way, with
    /// `latest_version: "unknown"` and `available: false` when it could not be
    /// resolved.
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
                // Guarded on having resolved something, so a kind that could not
                // be reached never shows a phantom update to a version named
                // "unknown".
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
    /// Degrades per kind for the same reason [`Self::check`] does, and emits a
    /// row for every kind ASKED FOR rather than for every kind it could resolve
    /// (`manager.ts:379-408`): the confirmation modal lists what the user is
    /// about to accept, so a kind silently missing from the answer is a download
    /// that happens without appearing in the list that consented to it.
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
            // A pinned entry carries no size, so the modal falls back to a HEAD
            // on the resolved URL (`manager.ts:396`). A resolver entry already
            // learned it from the release asset.
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
    /// The usual cause is the in-app update flow: the install kicks off sidecar
    /// downloads and then triggers the core self-update, whose handoff to the
    /// updater tears this process down mid-download.
    ///
    /// The queue can resume the BYTES, and that is not enough: the
    /// extract-into-bin plus record-the-version half lives in the task
    /// [`Self::kickoff`] spawned, which died with the old process, and a
    /// generically resumed row re-verifies no sha256 either. So
    /// [`DownloadQueue::resume_pending`] skips `binaries` rows and this re-runs
    /// the whole kickoff instead: re-resolve, re-download, verify, extract,
    /// record. Without it the row sits `Pending` forever with nothing alive to
    /// advance it, the staged archive is orphaned, and the requirements gate
    /// can never be satisfied.
    ///
    /// Best-effort by design: a per-kind failure is recorded as a retryable
    /// requirements gate and never blocks boot. Cache-first, so a boot with no
    /// interrupted work makes no request at all.
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
            // so leaving this one would strand the core status bar on
            // "Downloading" with no worker behind it forever. `remove` no-ops on
            // an in-flight row, and `resume_pending` skipped binaries, so
            // nothing here is in flight.
            if let Err(err) = self.inner.queue.remove(&row.id).await {
                tracing::warn!(target: "loca::downloads", "reconcile: dropping {}: {err}", row.id);
            }
        }
        if kinds.is_empty() {
            return;
        }

        // Cache-first: the manifest was fetched and cached during the original
        // consented update, so re-initiating uses it with no new network. Only
        // reached when there IS interrupted binary work.
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
        // A fresh attempt clears the previous reason, so a retry that is still
        // running does not report the old failure as current.
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
    /// what they become. It is also why a `binaries` row is never resumed by the
    /// generic queue: this step lives in the process that started it.
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
        // The staged archive has served its purpose; leaving it behind would
        // keep a full copy of every installed binary on disk forever.
        let _ = tokio::fs::remove_file(&archive).await;
        self.write_installed_version(kind, &resolved.version)
            .await?;
        self.clear_failure(kind);
        Ok(())
    }

    async fn write_installed_version(&self, kind: BinaryKind, version: &str) -> Result<()> {
        let mut installed = self.read_installed().await;
        installed.insert(
            kind.to_string(),
            InstalledVersion {
                version: version.to_string(),
                // BOTH, always. See the module docs: a client judges presence by
                // `target`, so recording only `variant` makes every install look
                // like a degraded one.
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
    /// It does NOT go through `load(false)`. That call short-circuits only on a
    /// GOOD cache, and a cache read returns `None` for four separate reasons
    /// (absent, unreadable, malformed, failing verification), so every one of
    /// the other three fell through to `fetch_and_verify`. Guarding on the file
    /// merely EXISTING closes one of the four and reads as if it closed all of
    /// them, which is how a route documented as network-free reached the
    /// network on a truncated cache.
    async fn cached_manifest(&self) -> Option<BinaryManifest> {
        if self.inner.channel == Channel::Dev {
            return Some(crate::downloads::binaries::manifest::dev_manifest());
        }
        crate::downloads::binaries::manifest::cached_manifest(&self.inner.paths.cache_dir()).await
    }
}

/// The `group_id` every row a single kind's install creates is tagged with.
///
/// The one thing that ties an orphaned row back to the kind that started it,
/// which is what [`BinariesInstaller::reconcile_interrupted_installs`] reads on
/// boot. Minted and parsed in one place so the two cannot drift.
pub fn group_id_for(kind: BinaryKind) -> String {
    format!("{GROUP_ID_PREFIX}{kind}")
}

fn kind_from_group_id(group_id: &str) -> Option<BinaryKind> {
    match group_id.strip_prefix(GROUP_ID_PREFIX)? {
        "deno" => Some(BinaryKind::Deno),
        "llama-server" => Some(BinaryKind::LlamaServer),
        "loca-core-speech" => Some(BinaryKind::LocaCoreSpeech),
        _ => None,
    }
}

const GROUP_ID_PREFIX: &str = "binaries:";

/// What a version reads as when it could not be resolved. The oracle's literal
/// (`manager.ts:331`), and load-bearing rather than cosmetic: it is what lets a
/// degraded row still be a row.
const UNKNOWN_VERSION: &str = "unknown";

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
        // D-028: absent means "everything missing", empty means "nothing". A
        // client that sends [] must not trigger a full install.
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
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(binary_name(BinaryKind::Deno)), b"x").unwrap();
        assert!(installer.missing_kinds().await.is_empty());
    }

    #[tokio::test]
    async fn a_failed_install_is_visible_to_the_requirements_snapshot() {
        // Swallowing the error leaves the user with a binary that is simply
        // absent, no reason given, and a retry that reports nothing.
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

    #[tokio::test]
    async fn one_kind_failing_does_not_block_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);

        // Both are attempted; only the unsupported one records a failure.
        installer
            .install(Some(vec![
                BinaryKind::LlamaServer,
                BinaryKind::LocaCoreSpeech,
            ]))
            .await
            .unwrap();

        assert!(installer.failure(BinaryKind::LlamaServer).is_some());
        assert!(installer.failure(BinaryKind::LocaCoreSpeech).is_some());
    }

    #[tokio::test]
    async fn list_reports_the_variant_and_the_target_it_aimed_for() {
        // A client judges presence by `target == desiredVariant`, so an install
        // that records `variant` and not `target` reads as perpetually missing
        // and re-offers itself in the requirements popup forever.
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(binary_name(BinaryKind::Deno)), b"x").unwrap();
        installer
            .write_installed_version(BinaryKind::Deno, "v2.8.2")
            .await
            .unwrap();

        let listed = installer.list().await.unwrap();
        let deno = listed
            .iter()
            .find(|s| s.kind == BinaryKind::Deno)
            .expect("deno is on the contract");

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
        let deno = listed.iter().find(|s| s.kind == BinaryKind::Deno).unwrap();

        assert!(!deno.installed);
        assert_eq!(deno.path, None);
        assert_eq!(deno.variant, None);
        assert_eq!(deno.target, None);
    }

    #[tokio::test]
    async fn list_covers_every_kind_on_the_contract_even_the_uninstallable_ones() {
        // The wire type stays 3-valued until cutover, so a client that renders
        // all three must still get all three rather than a short list.
        let dir = tempfile::tempdir().unwrap();
        let installer = installer(&dir);

        let listed = installer.list().await.unwrap();

        assert_eq!(listed.len(), 3);
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
        // The finding, and it breaks the repo's one ABSOLUTE rule. The test
        // above runs on `Channel::Dev`, which returns an in-code manifest
        // before the cache is consulted at all, so it cannot reach the branch
        // it names. On a shipping channel the guard was "does the cache FILE
        // exist", and a cache read returns `None` for four separate reasons:
        // absent, unreadable, malformed, or failing verification. Three of the
        // four therefore fell through to `fetch_and_verify`, so a truncated
        // cache made `GET /api/v1/binaries` reach the internet.
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

                assert_eq!(listed.len(), 3, "{channel} / {what}");
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
        // The other half of the same route: a stable entry knows its version,
        // so a list that reports "unknown" is hiding what the client needs to
        // decide whether an install is even offered.
        let dir = tempfile::tempdir().unwrap();
        let trust = Trusted::new();
        let installer = installer_on(&dir, Channel::Stable);
        stage_cache(&installer, &trust, published_stable_manifest()).await;

        let listed = installer.list().await.unwrap();
        let deno = listed.iter().find(|s| s.kind == BinaryKind::Deno).unwrap();

        assert_eq!(deno.version, "v2.8.2");
        assert_eq!(installer.inner.net.calls(), 0);
    }

    #[tokio::test]
    async fn a_resolver_manifest_reports_latest_until_something_is_installed() {
        // `manager.ts:293-297`. A resolver entry does not know its version
        // without asking GitHub, and asking is what the consent rule forbids.
        let dir = tempfile::tempdir().unwrap();
        let trust = Trusted::new();
        let installer = installer_on(&dir, Channel::Latest);
        stage_cache(&installer, &trust, published_latest_manifest()).await;

        let listed = installer.list().await.unwrap();
        assert_eq!(
            listed
                .iter()
                .find(|s| s.kind == BinaryKind::Deno)
                .unwrap()
                .version,
            "latest"
        );

        installer
            .write_installed_version(BinaryKind::Deno, "v2.8.1")
            .await
            .unwrap();
        let listed = installer.list().await.unwrap();
        assert_eq!(
            listed
                .iter()
                .find(|s| s.kind == BinaryKind::Deno)
                .unwrap()
                .version,
            "v2.8.1",
            "an installed resolver entry reports what is on disk"
        );
        assert_eq!(installer.inner.net.calls(), 0);
    }

    // -- check and probe degrade per kind -----------------------------------

    #[tokio::test]
    async fn check_still_answers_when_upstream_cannot_be_reached() {
        // The oracle wraps each kind in try/catch, logs, and still pushes a row
        // (`manager.ts:340-350`). Unauthenticated GitHub is rate-limited to
        // about 60 requests an hour per IP, so returning an error envelope here
        // turns a normal day into a broken "check for updates" button where the
        // oracle says "nothing available". Offline does the same.
        let trust = Trusted::new();
        // Port 1 never listens, so the release lookup fails at connect.
        let _github = GithubHost::set("http://127.0.0.1:1");
        let manifest = Upstream::start(Serve::Json(trust.sign(published_latest_manifest()))).await;
        let dir = tempfile::tempdir().unwrap();
        let (installer, _queue) =
            installer_with(&dir, Channel::Latest, &manifest.url("/binaries.json"));
        // INSTALLED, with a recorded version. Without that the `available`
        // half of this test cannot fail: a kind that is not on disk is not
        // "available" under any implementation, so the guard that stops the
        // phantom update would be untested on a fixture that omits it.
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(binary_name(BinaryKind::Deno)), b"x").unwrap();
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
        // The confirmation modal lists what the user is about to accept, so a
        // kind silently missing from the answer is a download that happens
        // without appearing in the list that consented to it
        // (`manager.ts:379-408`).
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
        // A pinned (stable) entry carries no size, so the modal falls back to a
        // HEAD on the resolved URL (`manager.ts:396`). Without it every stable
        // download confirmation shows no size at all.
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
        // `resume_pending` skips `binaries` rows deliberately, and that skip is
        // only safe because something else finishes them. Nothing did: the row
        // sat `Pending` forever with the staged archive orphaned and the
        // requirements gate permanently unsatisfiable.
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
        // Cache-first and work-first: a boot with a settled queue must not turn
        // into an outbound request.
        let dir = tempfile::tempdir().unwrap();
        let installer = installer_on(&dir, Channel::Stable);

        installer.reconcile_interrupted_installs().await;

        assert_eq!(installer.inner.net.calls(), 0);
    }

    #[tokio::test]
    async fn reconcile_leaves_a_models_row_alone() {
        // It is the BINARIES half of the boot sequence; `start` already owns
        // the models rows, and touching them here would double-resume them.
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
        // The only thing tying an orphaned row back to its kind, so the minting
        // and the parsing have to be one pair rather than two string literals.
        for kind in [
            BinaryKind::Deno,
            BinaryKind::LlamaServer,
            BinaryKind::LocaCoreSpeech,
        ] {
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
        assert_eq!(installer.list().await.unwrap().len(), 3);
    }
}
