//! The install jobs: the two user-triggered phases stitched into something
//! runnable, plus the update that runs both under one job id.
//!
//! Ports `packages/loca-core/src/extensions/installer.ts`, the orchestration
//! half of `installer-download.ts`, `installer-deps.ts`'s `runInstallDeps`, and
//! `installer-register.ts`'s `finishRegister`.
//!
//! # Why the phases stay apart
//!
//! Download acquires bytes and validates `loca.json`. It runs no code. Install
//! spawns `deno`, which resolves third-party packages off the network. Keeping
//! them apart is what lets the built-in be SEEDED offline at first boot and
//! INSTALLED later, on a user's press, and it is why a seeded extension lands
//! at `downloaded` rather than `installed`.
//!
//! # Every job is fire-and-forget, and the guard outlives the caller
//!
//! `start_*` returns `{ jobId, extensionId }` as soon as the job is claimed and
//! reports the rest over WS. The per-extension claim is therefore held by the
//! spawned task rather than by the caller's stack frame: all three jobs derive
//! the same `.new` and `.old` staging paths from the extension id, so a second
//! job starting while the first is mid-swap wipes a half-written extract and
//! strands the previous version with no install directory at all.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::config::{Channel, Config};
use crate::contract::api::{
    ExtensionSource, ExtensionStatus, ServerToClientFrame, ServerToClientFrameStream,
};
use crate::contract::inbound::{ExtensionManifest, MemoryDecl};
use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;
use crate::sidecars::{deno, Manager, Output, Spawn, Stdin};

use super::super::hash::{hash_extension, sha256_hex};
use super::super::registry::{ExtensionInsert, Registry};
use super::net::{as_code, Http};
use super::{deps, download, manifest, npm, register, seed, InFlight, InstallSource, Planted};

/// What a `start_*` hands back before the work has happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallStarted {
    pub job_id: String,
    pub extension_id: String,
}

/// Something to run when a job has SUCCEEDED, before its `done` is reported.
///
/// One caller: boot seeding, whose per-extension marker file records that the
/// seed LANDED. The oracle expresses the same thing by wrapping the event sink
/// for the duration of one seed (`seeding.ts:92-98`); a hook on the job is that
/// without a second sink implementation whose only difference is a side effect.
///
/// It runs BEFORE `done` rather than after, so a client that reacts to
/// `extension.install_done` by re-reading state cannot observe a finished
/// install whose marker has not been written yet.
pub type OnSuccess =
    Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send>;

/// Where a job's progress goes.
///
/// A trait rather than the frame bus itself, so the whole install chain is
/// drivable without a transport. F6 owns the hub; this package produces the
/// payloads.
pub trait InstallEvents: Send + Sync {
    fn log(&self, job_id: &str, extension_id: &str, stream: ServerToClientFrameStream, line: &str);
    fn done(&self, job_id: &str, extension_id: &str, ok: bool, code: i64);
}

/// The read-only memories an extension ships.
///
/// Implemented by D5. Declared here because the consumer declares the trait, so
/// `extensions::` imports no `memories::` and D4 can call into either without a
/// cycle.
#[async_trait]
pub trait ExtensionMemories: Send + Sync {
    /// Register (or re-register) the memories one extension declares.
    ///
    /// `base_dir` is the PARENT of the install directory, so each row's
    /// `{extension_id}/{path}` filename resolves back inside it.
    async fn register_extension_memories(
        &self,
        extension_id: &str,
        base_dir: &Path,
        memories: &[MemoryDecl],
    ) -> Result<()>;

    /// Drop the memories one extension provided, when it is deleted.
    async fn remove_extension_memories(&self, extension_id: &str);

    /// Index what was just registered, rather than waiting for the next boot.
    fn schedule_indexing(&self);
}

/// The [`InstallEvents`] that puts an install's progress on the wire.
///
/// The installer reports through a trait rather than through the bus itself, so
/// the whole install chain is drivable with no transport; this is the
/// implementation that puts the frames on one. [`SilentEvents`] is the other,
/// and it is what a test and a mobile core get.
///
/// Every install is core-global rather than the requesting client's business:
/// two clients paired to one core are looking at the same installed set, so both
/// modals stream, and the one that did not press the button still refreshes.
///
/// It lives HERE, next to the trait, rather than next to `FrameBus`, and that is
/// the one seam in this crate that breaks the pattern deliberately (D-091). The
/// other seams put the impl in the module owning the underlying thing, because
/// there the edge runs one way. This one would not: `extensions` already holds
/// an `Arc<FrameBus>`, so a `transport -> extensions` impl closes the crate's
/// only module cycle. Hosting it here adds no edge at all.
pub struct BroadcastInstallEvents {
    frames: Arc<crate::transport::frame_bus::FrameBus>,
}

impl BroadcastInstallEvents {
    pub fn new(frames: Arc<crate::transport::frame_bus::FrameBus>) -> Self {
        Self { frames }
    }
}

impl InstallEvents for BroadcastInstallEvents {
    fn log(&self, job_id: &str, extension_id: &str, stream: ServerToClientFrameStream, line: &str) {
        self.frames
            .broadcast_all(&ServerToClientFrame::ExtensionInstallLog {
                id: extension_id.to_string(),
                job_id: job_id.to_string(),
                line: line.to_string(),
                stream,
            });
    }

    fn done(&self, job_id: &str, extension_id: &str, ok: bool, code: i64) {
        self.frames
            .broadcast_all(&ServerToClientFrame::ExtensionInstallDone {
                code,
                id: extension_id.to_string(),
                job_id: job_id.to_string(),
                ok,
            });
        // The snapshot is part of `done`, not a separate concern a caller could
        // forget: without it the installed list only refreshes when something
        // else happens to poll, and the modal closes onto a stale page.
        self.frames
            .broadcast_all(&ServerToClientFrame::ExtensionSnapshot);
    }
}

/// An [`InstallEvents`] that goes nowhere. Boot supplies the real one.
pub struct SilentEvents;

impl InstallEvents for SilentEvents {
    fn log(
        &self,
        _job_id: &str,
        _extension_id: &str,
        _stream: ServerToClientFrameStream,
        _line: &str,
    ) {
    }
    fn done(&self, _job_id: &str, _extension_id: &str, _ok: bool, _code: i64) {}
}

/// An [`ExtensionMemories`] for a core with no memories store wired.
///
/// Doing nothing is the CORRECT behaviour rather than a stub: with no store
/// there is nowhere to register to, and an extension whose tools work but whose
/// shipped knowledge is absent is exactly what a core without memories is.
pub struct NoMemories;

#[async_trait]
impl ExtensionMemories for NoMemories {
    async fn register_extension_memories(
        &self,
        _extension_id: &str,
        _base_dir: &Path,
        _memories: &[MemoryDecl],
    ) -> Result<()> {
        Ok(())
    }
    async fn remove_extension_memories(&self, _extension_id: &str) {}
    fn schedule_indexing(&self) {}
}

/// Everything the two phases need.
#[derive(Clone)]
pub struct Installer {
    inner: Arc<Inner>,
}

struct Inner {
    paths: Paths,
    config: Config,
    /// Read for `extensions.ignorePostinstallScripts` at install time rather
    /// than captured at construction, so turning the block off takes effect on
    /// the next install instead of the next restart.
    settings: crate::settings::Settings,
    registry: Registry,
    http: Arc<dyn Http>,
    sidecars: Arc<Manager>,
    memories: Arc<dyn ExtensionMemories>,
    events: Arc<dyn InstallEvents>,
    inflight: Arc<InFlight>,
}

/// [`Installer::new`]'s arguments, named.
///
/// A struct rather than a positional list, following `ChatConfig`: eight of
/// these are heterogeneous and four are `Arc<dyn _>`, so a transposition is a
/// type error only when the two happen to differ. Adding `settings` to the
/// positional form silently misaligned three arguments at one call site, which
/// is the argument for this shape rather than a hypothetical one.
pub struct InstallerConfig {
    pub paths: Paths,
    pub config: Config,
    /// Read for `extensions.ignorePostinstallScripts` at install time.
    pub settings: crate::settings::Settings,
    pub registry: Registry,
    pub http: Arc<dyn Http>,
    pub sidecars: Arc<Manager>,
    pub memories: Arc<dyn ExtensionMemories>,
    pub events: Arc<dyn InstallEvents>,
}

impl Installer {
    pub fn new(cfg: InstallerConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                paths: cfg.paths,
                config: cfg.config,
                settings: cfg.settings,
                registry: cfg.registry,
                http: cfg.http,
                sidecars: cfg.sidecars,
                memories: cfg.memories,
                events: cfg.events,
                inflight: Arc::new(InFlight::new()),
            }),
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.inner.registry
    }

    pub fn paths(&self) -> &Paths {
        &self.inner.paths
    }

    pub fn channel(&self) -> Channel {
        self.inner.config.channel
    }

    pub fn http(&self) -> &dyn Http {
        self.inner.http.as_ref()
    }

    pub fn is_busy(&self, extension_id: &str) -> bool {
        self.inner.inflight.is_busy(extension_id)
    }

    /// Phase 1, in the background.
    pub fn start_download(&self, spec: InstallSource) -> Result<InstallStarted> {
        self.start(
            spec.extension_id(),
            move |this, id, job| async move { this.run_download(spec, &id, &job).await },
            None,
        )
    }

    /// Phase 1, in the background, with something to run if it succeeds.
    ///
    /// Boot seeding is the only caller: its marker must be written when the
    /// install landed, never when it started (a failed first boot would then
    /// never retry) and never unconditionally afterwards (a user who deleted the
    /// extension would get it back).
    pub fn start_download_then(
        &self,
        spec: InstallSource,
        on_success: OnSuccess,
    ) -> Result<InstallStarted> {
        self.start(
            spec.extension_id(),
            move |this, id, job| async move { this.run_download(spec, &id, &job).await },
            Some(on_success),
        )
    }

    /// Phase 2, in the background.
    pub fn start_install_deps(&self, extension_id: &str) -> Result<InstallStarted> {
        self.start(
            extension_id.to_string(),
            move |this, id, job| async move { this.run_install_deps(&id, &job).await },
            None,
        )
    }

    /// Re-download and then re-install, under ONE job id.
    ///
    /// Both halves under one job because the install phase is what re-pins the
    /// content hash: a download that stopped there would leave the extension at
    /// `downloaded` with a stale pin, and the next verify would call a
    /// legitimate update content drift.
    pub fn start_update(&self, spec: InstallSource) -> Result<InstallStarted> {
        self.start(
            spec.extension_id(),
            move |this, id, job| async move {
                this.run_download(spec, &id, &job).await?;
                this.run_install_deps(&id, &job).await
            },
            None,
        )
    }

    /// Claim the extension, spawn the job, and report its outcome.
    ///
    /// The claim is taken BEFORE the task is spawned, so a caller that gets
    /// `Ok` back knows the second concurrent press was refused rather than
    /// merely scheduled behind it.
    fn start<F, Fut>(
        &self,
        extension_id: String,
        job: F,
        on_success: Option<OnSuccess>,
    ) -> Result<InstallStarted>
    where
        F: FnOnce(Installer, String, String) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send,
    {
        let job_id = ulid::Ulid::new().to_string();
        let guard = self.inner.inflight.clone().claim_owned(&extension_id)?;
        let this = self.clone();
        let id = extension_id.clone();
        let job_id_for_task = job_id.clone();
        tokio::spawn(async move {
            let outcome = job(this.clone(), id.clone(), job_id_for_task.clone()).await;
            match outcome {
                Ok(()) => {
                    // Before the frame, not after: a client that reacts to
                    // `install_done` by re-reading state would otherwise be able
                    // to observe a finished install whose hook had not run.
                    if let Some(on_success) = on_success {
                        on_success().await;
                    }
                    this.inner.events.done(&job_id_for_task, &id, true, 0);
                }
                Err(err) => {
                    tracing::error!(
                        target: "loca::extensions",
                        "install job for {id} failed: {}", err.message,
                    );
                    this.inner.events.done(&job_id_for_task, &id, false, 1);
                }
            }
            // Released once the job is completely finished, its success hook
            // included, and on the failure path as well as the success one.
            // Without the failure half the user presses Install again and is
            // told something that finished ten minutes ago is still in progress,
            // until the next restart; releasing it before the hook would let a
            // user-triggered download start while a seed's marker write is still
            // in flight.
            drop(guard);
        });
        Ok(InstallStarted {
            job_id,
            extension_id,
        })
    }

    /// Phase 1: acquire the files, validate `loca.json`, write the rows.
    pub async fn run_download(
        &self,
        spec: InstallSource,
        extension_id: &str,
        job_id: &str,
    ) -> Result<()> {
        let install_path = self.inner.registry.install_path(extension_id);
        // Defence in depth. The local slug charset and the npm name flattening
        // both already constrain this; the check covers whatever a future third
        // source resolves to as well.
        if !crate::contain::is_within(&self.inner.paths.extensions_dir(), &install_path) {
            return Err(AppError::new(
                ErrorCode::ValidationError,
                format!("extension \"{extension_id}\" resolves outside the extensions dir"),
            ));
        }
        let staging = download::with_suffix(&install_path, ".new");
        download::rm_rf(&staging)?;

        match self.acquire(&spec, &staging, extension_id, job_id).await {
            Ok(version) => {
                match self
                    .register_acquired(&spec, extension_id, &version, &staging, &install_path)
                    .await
                {
                    Ok(()) => Ok(()),
                    Err(err) => {
                        // The swap has already happened by here, so the staging
                        // directory is gone; clearing it again is harmless and
                        // keeps the one cleanup path.
                        let _ = download::rm_rf(&staging);
                        Err(err)
                    }
                }
            }
            Err(err) => {
                // A half-written extract must not survive: the next attempt
                // pre-clears anyway, but a failed download that leaves gigabytes
                // of `.new` behind is a disk leak the user cannot see.
                let _ = download::rm_rf(&staging);
                Err(err)
            }
        }
    }

    /// Validate, swap into place, and write the rows.
    async fn register_acquired(
        &self,
        spec: &InstallSource,
        extension_id: &str,
        version: &str,
        staging: &Path,
        install_path: &Path,
    ) -> Result<()> {
        // Validated at the STAGING root, before the swap: an extension with no
        // manifest never reaches the install directory, so a failed download
        // cannot replace a working extension with an unusable one.
        let raw = read_optional(&staging.join("loca.json"))
            .await
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::NoLocaJson,
                    format!("no loca.json at root of {extension_id}"),
                )
            })?;
        let parsed = parse_manifest_or_err(&raw)?;
        let manifest_hash = sha256_hex(raw.as_bytes());

        download::swap_into_place(staging, install_path, extension_id)?;
        tracing::info!(target: "loca::extensions", "downloaded {extension_id}@{version}");
        self.finish_register(
            extension_id,
            source_of(spec),
            version,
            install_path,
            &parsed,
            &manifest_hash,
        )
        .await
    }

    /// Get the bytes into `staging` and answer which version they are.
    async fn acquire(
        &self,
        spec: &InstallSource,
        staging: &Path,
        extension_id: &str,
        job_id: &str,
    ) -> Result<String> {
        tokio::fs::create_dir_all(staging).await?;
        match spec {
            InstallSource::Npm { name, version } => {
                let resolved =
                    npm::resolve_version(self.inner.http.as_ref(), name, version.as_deref())
                        .await?;
                self.log(
                    job_id,
                    extension_id,
                    format!("resolved {name}@{}", resolved.version),
                );
                let bytes = self.fetch_tarball(&resolved.tarball_url).await?;
                download::verify_tarball(
                    &bytes,
                    &resolved.tarball_url,
                    &download::Integrity {
                        sha256: None,
                        integrity: resolved.integrity.clone(),
                        shasum: resolved.shasum.clone(),
                    },
                )?;
                download::extract_tarball(&bytes, staging)?;
                Ok(resolved.version)
            }
            InstallSource::Local { path, .. } => {
                download::copy_tree_excluding_node_modules(Path::new(path), staging)?;
                Ok("local".to_string())
            }
            InstallSource::Seeded { id, planted } => {
                self.acquire_seeded(id, planted.as_deref(), staging, job_id)
                    .await
            }
        }
    }

    async fn acquire_seeded(
        &self,
        id: &str,
        planted: Option<&Planted>,
        staging: &Path,
        job_id: &str,
    ) -> Result<String> {
        let ext = seed::seeded_by_id(id).ok_or_else(|| {
            AppError::new(
                ErrorCode::ValidationError,
                format!("unknown seeded extension \"{id}\""),
            )
        })?;
        // The chokepoint the boot loop's own skip does not cover: a dev-only
        // extension must not arrive through a user-triggered download or update
        // either.
        seed::assert_installable(ext, self.inner.config.channel)?;

        if self.inner.config.channel == Channel::Dev {
            self.log(
                job_id,
                id,
                format!("installing seeded extension {id} from codebase"),
            );
            let source = codebase_path(ext.dir);
            download::copy_tree_excluding_node_modules(&source, staging)?;
            return dev_version(&source);
        }

        // First-boot seeding. The bytes were fetched and verified during the
        // INSTALL phase and planted on disk; this re-verifies them against the
        // sha256 carried inside the already-signature-checked manifest and
        // extracts. Nothing here may reach the network, and nothing here can:
        // there is no fetch on this path at all.
        if let Some(planted) = planted {
            let bytes = tokio::fs::read(&planted.tarball_path).await.map_err(|_| {
                AppError::new(
                    ErrorCode::TarballFetchFailed,
                    format!(
                        "planted {id} tarball not found at {}",
                        planted.tarball_path.display()
                    ),
                )
            })?;
            download::verify_tarball(
                &bytes,
                &planted.tarball_path.to_string_lossy(),
                &download::Integrity {
                    sha256: Some(planted.manifest.sha256.clone()),
                    integrity: None,
                    shasum: None,
                },
            )?;
            self.log(
                job_id,
                id,
                format!("installing seeded extension {id} from planted tarball"),
            );
            download::extract_tarball(&bytes, staging)?;
            return Ok(planted.manifest.version.clone());
        }

        // A user pressed Install or Update on a seeded extension, so fetching
        // the signed manifest and its tarball is authorised.
        let chosen = self.load_seeded_manifest(ext, true).await?;
        self.log(
            job_id,
            id,
            format!("downloading seeded extension {id} {}", chosen.version),
        );
        let bytes = self.fetch_tarball(&chosen.tarball_url).await?;
        download::verify_tarball(
            &bytes,
            &chosen.tarball_url,
            &download::Integrity {
                sha256: Some(chosen.sha256.clone()),
                integrity: None,
                shasum: None,
            },
        )?;
        download::extract_tarball(&bytes, staging)?;
        Ok(chosen.version)
    }

    /// The signed manifest for a seeded extension, from the cache or the CDN.
    ///
    /// `force` skips the cache, which is what an update check wants: a cached
    /// manifest answers the question "what did we last see", and the button the
    /// user pressed asks "what is there now".
    pub async fn load_seeded_manifest(
        &self,
        ext: &seed::SeededExtension,
        force: bool,
    ) -> Result<manifest::SeededManifest> {
        if self.inner.config.channel == Channel::Dev {
            let source = codebase_path(ext.dir);
            let version = dev_version(&source)?;
            return Ok(manifest::SeededManifest {
                schema_version: 1,
                version,
                id: ext.id.to_string(),
                tarball_url: String::new(),
                sha256: String::new(),
                signature: String::new(),
            });
        }
        let cache_dir = self.inner.paths.cache_dir();
        let cached = manifest::read_cached(&cache_dir, ext.id).await;
        if let Some(cached) = cached.clone() {
            if !force {
                return Ok(cached);
            }
        }
        let raw = self
            .inner
            .http
            .get_text(&self.inner.config.builtin_extension_manifest_url())
            .await?;
        let fetched = manifest::verify_document(&raw)?;
        let chosen = manifest::choose(fetched, cached.as_ref(), ext.id)?;
        if chosen.store {
            manifest::write_cached(&cache_dir, ext.id, &raw).await?;
        }
        Ok(chosen.manifest)
    }

    async fn fetch_tarball(&self, url: &str) -> Result<Vec<u8>> {
        self.inner
            .http
            .get_bytes(url)
            .await
            .map_err(|err| as_code(err, ErrorCode::TarballFetchFailed))
    }

    /// Write the rows for a freshly acquired extension.
    ///
    /// An extension declaring NO dependencies has nothing to install, so it
    /// pins its hash and reaches `installed` here. One with dependencies stays
    /// at `downloaded` until the explicit Install step.
    pub async fn finish_register(
        &self,
        extension_id: &str,
        source: ExtensionSource,
        version: &str,
        install_path: &Path,
        parsed: &ExtensionManifest,
        manifest_hash: &str,
    ) -> Result<()> {
        let has_deps = deps::has_declared_deps(install_path).await?;
        self.inner
            .registry
            .upsert_extension(ExtensionInsert {
                id: extension_id.to_string(),
                source,
                display_name: parsed.display_name.clone().into(),
                description: Some(parsed.description.clone().into()),
                version: version.to_string(),
                installed_path: install_path.to_string_lossy().into_owned(),
                manifest_hash: manifest_hash.to_string(),
                // Unpinned by download. The pin is what `installed` means, and
                // it happens once the folder is final.
                content_hash: String::new(),
                status: ExtensionStatus::Downloaded,
                has_deps,
                has_database: parsed.database,
            })
            .await?;
        self.inner
            .registry
            .replace_tools(extension_id, register::tool_rows(parsed))
            .await?;
        // The base dir is the PARENT of the install dir, so each memory row's
        // `{id}/{path}` filename resolves back under the install dir.
        let base_dir = install_path.parent().unwrap_or(install_path).to_path_buf();
        self.inner
            .memories
            .register_extension_memories(
                extension_id,
                &base_dir,
                parsed.memories.as_deref().unwrap_or(&[]),
            )
            .await?;
        self.inner.memories.schedule_indexing();

        if !has_deps {
            let hash = hash_extension(install_path)?;
            self.inner
                .registry
                .mark_installed(extension_id, &hash)
                .await?;
            tracing::info!(target: "loca::extensions", "installed {extension_id} (no deps)");
        }
        Ok(())
    }

    /// Register a folder the user dropped into the extensions directory.
    ///
    /// Used by rescan. No copy: the folder is already where it belongs.
    pub async fn register_local_downloaded(&self, extension_id: &str) -> Result<()> {
        let install_path = self.inner.registry.install_path(extension_id);
        let raw = read_optional(&install_path.join("loca.json"))
            .await
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::NoLocaJson,
                    format!("no loca.json at root of {extension_id}"),
                )
            })?;
        let parsed = parse_manifest_or_err(&raw)?;
        let manifest_hash = sha256_hex(raw.as_bytes());
        self.finish_register(
            extension_id,
            ExtensionSource::Local,
            "local",
            &install_path,
            &parsed,
            &manifest_hash,
        )
        .await
    }

    /// Phase 2: install the declared dependencies and pin the content hash.
    pub async fn run_install_deps(&self, extension_id: &str, job_id: &str) -> Result<()> {
        let extension = self.inner.registry.get_or_err(extension_id).await?;
        if extension.status == ExtensionStatus::Drift {
            return Err(AppError::new(
                ErrorCode::ExtensionHashDrift,
                format!(
                    "extension {extension_id} has drifted; confirm re-enable before installing"
                ),
            ));
        }
        let folder = PathBuf::from(&extension.installed_path);
        // In place, with no second swap: the download already put the folder in
        // its final location, no worker can spawn against a not-yet-installed
        // extension, and `node_modules` plus `deno.lock` are hash-excluded.
        if deps::has_declared_deps(&folder).await? {
            self.run_deno_install(&folder, extension_id, job_id).await?;
        }
        // Pinned AFTER the dependencies land, and it still equals the pristine
        // downloaded content because what `deno install` writes is excluded.
        let hash = hash_extension(&folder)?;
        self.inner
            .registry
            .mark_installed(extension_id, &hash)
            .await?;
        tracing::info!(target: "loca::extensions", "installed {extension_id}");
        Ok(())
    }

    async fn run_deno_install(
        &self,
        folder: &Path,
        extension_id: &str,
        job_id: &str,
    ) -> Result<()> {
        // Resolved rather than assumed, so an install before the runtime has
        // been downloaded says "download required files from Settings" instead
        // of failing with a bare ENOENT.
        let binary = deno::resolve_worker_deno(&self.inner.paths)?;
        let allow_scripts =
            deps::allow_postinstall_scripts(&self.inner.settings.load_resolved().await?);
        if allow_scripts {
            tracing::warn!(
                target: "loca::extensions",
                "{extension_id}: npm lifecycle scripts are ENABLED for this install \
                 (extensions.ignorePostinstallScripts is off)",
            );
        }
        let plan = deps::deps_install_plan(
            &binary,
            folder,
            &self.inner.paths.deno_cache_dir(),
            allow_scripts,
        );
        let child = self.inner.sidecars.spawn(
            Spawn::new(format!("deno install {extension_id}"), &plan.program)
                .args(&plan.args)
                .cwd(&plan.cwd)
                // The plan's environment is COMPLETE rather than additive, and
                // `Spawn` clears the inherited superset, so a key the plan does
                // not name is a key the install cannot read. That is the point:
                // `deno install` resolves third-party packages and the core's
                // environment can hold an operator's API keys.
                .envs(plan.env.iter().cloned().collect())
                .stdin(Stdin::Null)
                .stdout(Output::Piped)
                .stderr(Output::Piped),
        )?;

        let (stdout, stderr) = {
            let mut child = child.lock().await;
            (child.take_stdout(), child.take_stderr())
        };
        let out_task = self.pump(
            stdout,
            job_id,
            extension_id,
            ServerToClientFrameStream::Stdout,
        );
        let err_task = self.pump(
            stderr,
            job_id,
            extension_id,
            ServerToClientFrameStream::Stderr,
        );
        // Both streams are drained CONCURRENTLY with the wait: `deno install`
        // writes more than a pipe buffer holds, and draining one at a time
        // deadlocks against a child blocked writing the other.
        let (_, captured) = tokio::join!(out_task, err_task);
        let exit = child.lock().await.wait().await?;
        let code = exit.code.unwrap_or(-1);
        if code != 0 {
            return Err(deps::deps_failed(
                extension_id,
                code,
                &deps::stderr_tail(&captured),
            ));
        }
        Ok(())
    }

    /// Drain one of the child's streams line by line, forwarding each line and
    /// returning what was read.
    async fn pump<R>(
        &self,
        stream: Option<R>,
        job_id: &str,
        extension_id: &str,
        which: ServerToClientFrameStream,
    ) -> Vec<String>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        use tokio::io::AsyncBufReadExt;

        let Some(stream) = stream else {
            return Vec::new();
        };
        let mut lines = tokio::io::BufReader::new(stream).lines();
        let mut collected = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.is_empty() {
                continue;
            }
            self.inner
                .events
                .log(job_id, extension_id, which, line.as_str());
            collected.push(line);
        }
        collected
    }

    fn log(&self, job_id: &str, extension_id: &str, line: impl AsRef<str>) {
        self.inner.events.log(
            job_id,
            extension_id,
            ServerToClientFrameStream::Stdout,
            line.as_ref(),
        );
    }
}

fn source_of(spec: &InstallSource) -> ExtensionSource {
    match spec {
        InstallSource::Npm { .. } => ExtensionSource::Npm,
        InstallSource::Local { .. } => ExtensionSource::Local,
        InstallSource::Seeded { .. } => ExtensionSource::Seeded,
    }
}

/// Where a seeded extension's bytes live in the repo, for the dev channel.
///
/// Compile-time rather than runtime, exactly as the specification's
/// `import.meta.url` is: this path exists only in a checkout, and the dev
/// channel is the only one that reads it. A shipped binary never reaches here
/// because [`seed::assert_installable`] and the branch above both gate on the
/// channel.
fn codebase_path(dir: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(Path::new("."))
        .join(dir)
}

/// The dev channel's synthetic version: the package version plus a content
/// stamp, so an edit to a seeded extension in a checkout reads as a new version
/// rather than as the released one.
fn dev_version(dir: &Path) -> Result<String> {
    let package_version = std::fs::read_to_string(dir.join("deno.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|cfg| {
            cfg.get("version")
                .and_then(|v| v.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| "0.0.0".to_string());
    let content = hash_extension(dir)?;
    Ok(format!(
        "{package_version}+dev.{}",
        &content[..8.min(content.len())]
    ))
}

pub(super) async fn read_optional(path: &Path) -> Option<String> {
    tokio::fs::read_to_string(path).await.ok()
}

/// Parse `loca.json`.
///
/// The generated contract compiles the Zod schema into the deserializer, so
/// deserializing IS the validation and a second pass after it would be the
/// duplicate D-044 warns about.
pub fn parse_manifest_or_err(raw: &str) -> Result<ExtensionManifest> {
    serde_json::from_str(raw).map_err(|err| {
        AppError::new(
            ErrorCode::InvalidLocaJson,
            format!("invalid loca.json: {err}"),
        )
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use base64::prelude::{Engine as _, BASE64_STANDARD};
    use sha2::Digest as _;

    use crate::config::Config;
    use crate::db::Db;
    use crate::extensions::install::net::NoNetwork;

    use super::super::manifest::SeededManifest;

    /// Records every frame the job produced, so a test can assert on progress
    /// as well as on the rows.
    #[derive(Default)]
    struct Recorded {
        logs: Mutex<Vec<(String, String, String)>>,
        done: Mutex<Vec<(String, String, bool, i64)>>,
    }

    impl InstallEvents for Recorded {
        fn log(
            &self,
            job_id: &str,
            extension_id: &str,
            stream: ServerToClientFrameStream,
            line: &str,
        ) {
            let which = match stream {
                ServerToClientFrameStream::Stdout => "stdout",
                ServerToClientFrameStream::Stderr => "stderr",
            };
            self.logs.lock().unwrap().push((
                job_id.to_string(),
                which.to_string(),
                line.to_string(),
            ));
            let _ = extension_id;
        }
        fn done(&self, job_id: &str, extension_id: &str, ok: bool, code: i64) {
            self.done.lock().unwrap().push((
                job_id.to_string(),
                extension_id.to_string(),
                ok,
                code,
            ));
        }
    }

    /// Answers whatever it is currently pointed at.
    ///
    /// Re-pointable rather than fixed so a test can run TWO downloads against
    /// ONE installer, which is the only way to express "the second download
    /// served different bytes": the registry is not clonable, and a second
    /// installer over a second database would be testing two extensions rather
    /// than one being replaced.
    struct Canned {
        text: Mutex<String>,
        bytes: Mutex<Vec<u8>>,
    }

    impl Canned {
        fn new(text: impl Into<String>, bytes: Vec<u8>) -> Self {
            Self {
                text: Mutex::new(text.into()),
                bytes: Mutex::new(bytes),
            }
        }
        fn retarget(&self, text: impl Into<String>, bytes: Vec<u8>) {
            *self.text.lock().unwrap() = text.into();
            *self.bytes.lock().unwrap() = bytes;
        }
    }

    #[async_trait]
    impl Http for Canned {
        async fn get_text(&self, _url: &str) -> Result<String> {
            Ok(self.text.lock().unwrap().clone())
        }
        async fn get_bytes(&self, _url: &str) -> Result<Vec<u8>> {
            Ok(self.bytes.lock().unwrap().clone())
        }
    }

    struct Fixture {
        installer: Installer,
        events: Arc<Recorded>,
        _dir: tempfile::TempDir,
        paths: Paths,
    }

    fn fixture(http: Arc<dyn Http>) -> Fixture {
        fixture_on(http, Channel::Stable)
    }

    fn fixture_on(http: Arc<dyn Http>, channel: Channel) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path());
        std::fs::create_dir_all(paths.extensions_dir()).unwrap();
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| crate::db::migrate(conn)).unwrap();
        let settings = crate::settings::Settings::new(
            &paths,
            db.clone(),
            Arc::new(crate::transport::FrameBus::new()),
        );
        let registry = Registry::new(db, paths.clone());
        let mut config = Config::from_env().unwrap_or_else(|_| Config {
            channel,
            host: "127.0.0.1".to_string(),
            port: 1,
            version: "0.0.0",
            storage_base_url: "https://get.example.test".to_string(),
            hf_base_url: "https://hf.example.test".to_string(),
        });
        config.channel = channel;
        let events = Arc::new(Recorded::default());
        let installer = Installer::new(InstallerConfig {
            paths: paths.clone(),
            config,
            settings,
            registry,
            http,
            sidecars: Arc::new(Manager::new()),
            memories: Arc::new(NoMemories),
            events: events.clone(),
        });
        Fixture {
            installer,
            events,
            _dir: dir,
            paths,
        }
    }

    fn manifest_json(name: &str, tools: &str) -> String {
        format!(
            r#"{{"name":"{name}","displayName":"The {name}","description":"a test extension","tools":[{tools}]}}"#
        )
    }

    fn one_tool() -> &'static str {
        r#"{"name":"do_thing","description":"does a thing","function":"doThing","parameters":{"type":"object"}}"#
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, bytes).unwrap();
        gz.finish().unwrap()
    }

    /// A tarball in npm's shape: every entry under `package/`.
    fn tarball(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, body) in entries {
            let mut header = tar::Header::new_ustar();
            header.set_size(body.len() as u64);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("package/{path}"), body.as_bytes())
                .unwrap();
        }
        gzip(&builder.into_inner().unwrap())
    }

    fn sha256(bytes: &[u8]) -> String {
        hex::encode(sha2::Sha256::digest(bytes))
    }

    // --- the offline seed chain -------------------------------------------

    #[tokio::test]
    async fn a_planted_seed_installs_from_disk_and_never_reaches_the_network() {
        // Structural, not observational: the HTTP seam PANICS if anything
        // fetches. A test that ran the offline path and saw no error would
        // prove nothing, because the offline path is also what runs when
        // everything works.
        let fx = fixture(Arc::new(NoNetwork));
        let bytes = tarball(&[("loca.json", &manifest_json("builtin", one_tool()))]);
        let tarball_path = fx.paths.extensions_dir().join(".planted.tgz");
        std::fs::write(&tarball_path, &bytes).unwrap();

        let spec = InstallSource::Seeded {
            id: seed::BUILTIN_EXTENSION_ID.to_string(),
            planted: Some(Box::new(Planted {
                tarball_path,
                manifest: SeededManifest {
                    schema_version: 1,
                    version: "1.2.3".to_string(),
                    id: seed::BUILTIN_EXTENSION_ID.to_string(),
                    tarball_url: String::new(),
                    sha256: sha256(&bytes),
                    signature: String::new(),
                },
            })),
        };
        fx.installer
            .run_download(spec, seed::BUILTIN_EXTENSION_ID, "job-1")
            .await
            .unwrap();

        let row = fx
            .installer
            .registry()
            .get_or_err(seed::BUILTIN_EXTENSION_ID)
            .await
            .unwrap();
        assert_eq!(row.version, "1.2.3");
        assert_eq!(row.source, ExtensionSource::Seeded);
    }

    #[tokio::test]
    async fn a_planted_tarball_whose_bytes_were_swapped_is_refused_before_extraction() {
        let fx = fixture(Arc::new(NoNetwork));
        let bytes = tarball(&[("loca.json", &manifest_json("builtin", one_tool()))]);
        let tarball_path = fx.paths.extensions_dir().join(".planted.tgz");
        std::fs::write(&tarball_path, &bytes).unwrap();

        let spec = InstallSource::Seeded {
            id: seed::BUILTIN_EXTENSION_ID.to_string(),
            planted: Some(Box::new(Planted {
                tarball_path,
                manifest: SeededManifest {
                    schema_version: 1,
                    version: "1.2.3".to_string(),
                    id: seed::BUILTIN_EXTENSION_ID.to_string(),
                    tarball_url: String::new(),
                    // The sha256 the SIGNED manifest carries, against bytes
                    // that are not the ones it signed.
                    sha256: sha256(b"other bytes entirely"),
                    signature: String::new(),
                },
            })),
        };
        let err = fx
            .installer
            .run_download(spec, seed::BUILTIN_EXTENSION_ID, "job-1")
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ChecksumMismatch);
        assert!(
            !fx.paths.extension_dir(seed::BUILTIN_EXTENSION_ID).exists(),
            "a refused tarball must not leave an install directory behind"
        );
    }

    #[tokio::test]
    async fn a_missing_plant_fails_rather_than_falling_back_to_a_fetch() {
        // The fallback that would be natural to write here is the one the
        // whole no-non-consented-network rule forbids.
        let fx = fixture(Arc::new(NoNetwork));
        let spec = InstallSource::Seeded {
            id: seed::BUILTIN_EXTENSION_ID.to_string(),
            planted: Some(Box::new(Planted {
                tarball_path: fx.paths.extensions_dir().join(".absent.tgz"),
                manifest: SeededManifest {
                    schema_version: 1,
                    version: "1.2.3".to_string(),
                    id: seed::BUILTIN_EXTENSION_ID.to_string(),
                    tarball_url: String::new(),
                    sha256: String::new(),
                    signature: String::new(),
                },
            })),
        };
        let err = fx
            .installer
            .run_download(spec, seed::BUILTIN_EXTENSION_ID, "job-1")
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::TarballFetchFailed);
    }

    #[tokio::test]
    async fn a_dev_only_seeded_extension_cannot_be_installed_off_the_dev_channel() {
        // The boot loop skips it; this is the chokepoint that also covers the
        // user-triggered download and update routes.
        let fx = fixture(Arc::new(NoNetwork));
        let err = fx
            .installer
            .run_download(
                InstallSource::Seeded {
                    id: seed::SAMPLES_EXTENSION_ID.to_string(),
                    planted: None,
                },
                seed::SAMPLES_EXTENSION_ID,
                "job-1",
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(err.message.contains("dev-only"), "{}", err.message);
    }

    #[tokio::test]
    async fn an_unknown_seeded_id_is_refused_rather_than_treated_as_a_new_one() {
        let fx = fixture(Arc::new(NoNetwork));
        let err = fx
            .installer
            .run_download(
                InstallSource::Seeded {
                    id: "loca-extension-invented".to_string(),
                    planted: None,
                },
                "loca-extension-invented",
                "job-1",
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError);
    }

    // --- the npm path ------------------------------------------------------

    fn npm_metadata(name: &str, version: &str, tarball: &[u8]) -> String {
        serde_json::json!({
            "dist-tags": { "latest": version },
            "versions": {
                version: {
                    "dist": {
                        "tarball": format!("https://registry.npmjs.org/{name}/-/x.tgz"),
                        "integrity": format!(
                            "sha512-{}",
                            BASE64_STANDARD.encode(sha2::Sha512::digest(tarball))
                        )
                    }
                }
            }
        })
        .to_string()
    }

    #[tokio::test]
    async fn an_npm_download_lands_the_rows_and_the_files() {
        let bytes = tarball(&[
            ("loca.json", &manifest_json("weather", one_tool())),
            ("index.ts", "export default () => {};"),
        ]);
        let http = Arc::new(Canned::new(
            npm_metadata("weather", "3.0.0", &bytes),
            bytes.clone(),
        ));
        let fx = fixture(http);
        fx.installer
            .run_download(
                InstallSource::Npm {
                    name: "weather".to_string(),
                    version: None,
                },
                "weather",
                "job-1",
            )
            .await
            .unwrap();

        let row = fx.installer.registry().get_or_err("weather").await.unwrap();
        assert_eq!(row.version, "3.0.0");
        assert_eq!(row.source, ExtensionSource::Npm);
        // No declared dependencies, so there is nothing to install and the
        // extension reaches `installed` with its hash pinned here.
        assert_eq!(row.status, ExtensionStatus::Installed);
        assert!(
            !row.content_hash.is_empty(),
            "an installed extension is pinned"
        );
        assert!(fx.paths.extension_dir("weather").join("index.ts").exists());
        let tools = fx.installer.registry().list_tools("weather").await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "do_thing");
    }

    #[tokio::test]
    async fn an_extension_declaring_dependencies_stops_at_downloaded() {
        // Reaching `installed` here would mean an extension whose imports have
        // never been resolved is exposed to the model, and its first tool call
        // fails on a missing module.
        let bytes = tarball(&[
            ("loca.json", &manifest_json("deps", one_tool())),
            ("deno.json", r#"{"imports":{"zod":"npm:zod@3"}}"#),
        ]);
        let http = Arc::new(Canned::new(
            npm_metadata("deps", "1.0.0", &bytes),
            bytes.clone(),
        ));
        let fx = fixture(http);
        fx.installer
            .run_download(
                InstallSource::Npm {
                    name: "deps".to_string(),
                    version: None,
                },
                "deps",
                "job-1",
            )
            .await
            .unwrap();
        let row = fx.installer.registry().get_or_err("deps").await.unwrap();
        assert_eq!(row.status, ExtensionStatus::Downloaded);
        assert!(row.has_deps);
        assert!(
            row.content_hash.is_empty(),
            "a pin before the dependencies land would attest content that is about to change"
        );
    }

    #[tokio::test]
    async fn a_tarball_with_no_loca_json_never_replaces_the_installed_extension() {
        // Validation happens at the STAGING root, before the swap. Validating
        // after it would let a bad download destroy a working extension.
        let good = tarball(&[("loca.json", &manifest_json("thing", one_tool()))]);
        let http = Arc::new(Canned::new(
            npm_metadata("thing", "1.0.0", &good),
            good.clone(),
        ));
        let fx = fixture(http.clone());
        let npm = || InstallSource::Npm {
            name: "thing".to_string(),
            version: None,
        };
        fx.installer
            .run_download(npm(), "thing", "job-1")
            .await
            .unwrap();

        let bad = tarball(&[("readme.md", "no manifest here")]);
        http.retarget(npm_metadata("thing", "2.0.0", &bad), bad);
        let err = fx
            .installer
            .run_download(npm(), "thing", "job-2")
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NoLocaJson);
        assert!(
            fx.paths.extension_dir("thing").join("loca.json").exists(),
            "the working extension must survive a failed re-download"
        );
        let row = fx.installer.registry().get_or_err("thing").await.unwrap();
        assert_eq!(row.version, "1.0.0", "the row must not move either");
    }

    #[tokio::test]
    async fn a_failed_download_leaves_no_staging_directory_behind() {
        // Both failure points, because they are two different cleanup paths and
        // one test can only cover one of them. The staging directory is created
        // before anything is written into it, so ANY failure after that point
        // leaves it behind: a failed download that keeps its extract is a disk
        // leak the user cannot see, and on a large npm extension it is
        // gigabytes.
        //
        // (a) the ACQUIRE half fails: the bytes did not verify.
        let good = tarball(&[("loca.json", &manifest_json("thing", one_tool()))]);
        let http = Arc::new(Canned::new(
            // Metadata whose integrity belongs to different bytes.
            npm_metadata("thing", "1.0.0", b"other bytes entirely"),
            good.clone(),
        ));
        let fx = fixture(http.clone());
        let err = fx
            .installer
            .run_download(
                InstallSource::Npm {
                    name: "thing".to_string(),
                    version: None,
                },
                "thing",
                "job-1",
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ChecksumMismatch);
        assert!(
            !fx.paths.extensions_dir().join("thing.new").exists(),
            "a refused download left its staging directory behind"
        );

        // (b) the VALIDATE half fails: the bytes verified and carry no manifest.
        let bad = tarball(&[("readme.md", "no manifest here")]);
        http.retarget(npm_metadata("thing", "1.0.0", &bad), bad);
        let err = fx
            .installer
            .run_download(
                InstallSource::Npm {
                    name: "thing".to_string(),
                    version: None,
                },
                "thing",
                "job-2",
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NoLocaJson);
        assert!(
            !fx.paths.extensions_dir().join("thing.new").exists(),
            "a rejected manifest left its staging directory behind"
        );
    }

    #[tokio::test]
    async fn an_id_that_escapes_the_extensions_directory_is_refused() {
        let fx = fixture(Arc::new(NoNetwork));
        let err = fx
            .installer
            .run_download(
                InstallSource::Local {
                    path: "/tmp".to_string(),
                    slug: "..".to_string(),
                },
                "..",
                "job-1",
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(err.message.contains("outside"), "{}", err.message);
    }

    // --- the local path ----------------------------------------------------

    #[tokio::test]
    async fn a_local_folder_is_copied_and_registered_as_local() {
        let fx = fixture(Arc::new(NoNetwork));
        let source = fx._dir.path().join("src-ext");
        std::fs::create_dir_all(source.join("node_modules")).unwrap();
        std::fs::write(source.join("loca.json"), manifest_json("mine", one_tool())).unwrap();
        std::fs::write(source.join("node_modules/huge.js"), "x").unwrap();

        fx.installer
            .run_download(
                InstallSource::Local {
                    path: source.to_string_lossy().into_owned(),
                    slug: "mine".to_string(),
                },
                "mine",
                "job-1",
            )
            .await
            .unwrap();
        let row = fx.installer.registry().get_or_err("mine").await.unwrap();
        assert_eq!(row.source, ExtensionSource::Local);
        assert_eq!(row.version, "local");
        assert!(
            !fx.paths.extension_dir("mine").join("node_modules").exists(),
            "node_modules is excluded from the copy and from the hash"
        );
    }

    #[tokio::test]
    async fn a_dropped_in_folder_registers_without_being_copied() {
        let fx = fixture(Arc::new(NoNetwork));
        let folder = fx.paths.extension_dir("dropped");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("loca.json"),
            manifest_json("dropped", one_tool()),
        )
        .unwrap();

        fx.installer
            .register_local_downloaded("dropped")
            .await
            .unwrap();
        let row = fx.installer.registry().get_or_err("dropped").await.unwrap();
        assert_eq!(row.source, ExtensionSource::Local);
        assert_eq!(row.status, ExtensionStatus::Installed);
    }

    #[tokio::test]
    async fn a_folder_with_no_manifest_is_refused_by_rescan_rather_than_registered_empty() {
        let fx = fixture(Arc::new(NoNetwork));
        std::fs::create_dir_all(fx.paths.extension_dir("stray")).unwrap();
        let err = fx
            .installer
            .register_local_downloaded("stray")
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NoLocaJson);
    }

    #[tokio::test]
    async fn an_invalid_manifest_is_an_error_rather_than_an_extension_with_no_tools() {
        let fx = fixture(Arc::new(NoNetwork));
        let folder = fx.paths.extension_dir("broken");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("loca.json"), "{ not json").unwrap();
        let err = fx
            .installer
            .register_local_downloaded("broken")
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidLocaJson);
    }

    // --- phase 2 -----------------------------------------------------------

    #[tokio::test]
    async fn installing_a_drifted_extension_is_refused() {
        let fx = fixture(Arc::new(NoNetwork));
        let folder = fx.paths.extension_dir("drifty");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("loca.json"),
            manifest_json("drifty", one_tool()),
        )
        .unwrap();
        fx.installer
            .register_local_downloaded("drifty")
            .await
            .unwrap();
        fx.installer.registry().mark_drift("drifty").await.unwrap();

        let err = fx
            .installer
            .run_install_deps("drifty", "job-1")
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ExtensionHashDrift);
    }

    #[tokio::test]
    async fn a_dependency_free_install_pins_the_hash_without_spawning_deno() {
        // No `deno` exists under this fixture's bin directory, so a spawn here
        // would fail with `binary_not_found`. Reaching `installed` proves the
        // no-dependencies short circuit, not merely that the call returned.
        let fx = fixture(Arc::new(NoNetwork));
        let folder = fx.paths.extension_dir("plain");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("loca.json"), manifest_json("plain", one_tool())).unwrap();
        fx.installer
            .register_local_downloaded("plain")
            .await
            .unwrap();
        fx.installer
            .registry()
            .mark_downloaded("plain")
            .await
            .unwrap();

        fx.installer
            .run_install_deps("plain", "job-1")
            .await
            .unwrap();
        let row = fx.installer.registry().get_or_err("plain").await.unwrap();
        assert_eq!(row.status, ExtensionStatus::Installed);
        assert!(!row.content_hash.is_empty());
    }

    #[tokio::test]
    async fn an_install_before_the_runtime_is_downloaded_says_so() {
        let fx = fixture(Arc::new(NoNetwork));
        let folder = fx.paths.extension_dir("needsdeno");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("loca.json"),
            manifest_json("needsdeno", one_tool()),
        )
        .unwrap();
        std::fs::write(
            folder.join("deno.json"),
            r#"{"imports":{"z":"jsr:@std/path"}}"#,
        )
        .unwrap();
        fx.installer
            .register_local_downloaded("needsdeno")
            .await
            .unwrap();

        let err = fx
            .installer
            .run_install_deps("needsdeno", "job-1")
            .await
            .unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::BinaryNotFound,
            "a raw spawn ENOENT tells the user nothing about how to fix it"
        );
    }

    // --- the jobs ----------------------------------------------------------

    #[tokio::test]
    async fn a_started_job_reports_its_outcome_and_releases_its_claim() {
        let bytes = tarball(&[("loca.json", &manifest_json("jobby", one_tool()))]);
        let http = Arc::new(Canned::new(
            npm_metadata("jobby", "1.0.0", &bytes),
            bytes.clone(),
        ));
        let fx = fixture(http);
        let started = fx
            .installer
            .start_download(InstallSource::Npm {
                name: "jobby".to_string(),
                version: None,
            })
            .unwrap();
        assert_eq!(started.extension_id, "jobby");

        for _ in 0..200 {
            if !fx.events.done.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let done = fx.events.done.lock().unwrap().clone();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].0, started.job_id, "the done frame names its job");
        assert_eq!(done[0].1, "jobby");
        assert!(done[0].2, "the job succeeded");
        assert_eq!(done[0].3, 0);
        assert!(
            !fx.installer.is_busy("jobby"),
            "the claim must be released once the job ends"
        );
    }

    #[tokio::test]
    async fn a_job_that_fails_still_reports_done_and_releases_its_claim() {
        // Released on the failure path too. Without that the user presses
        // Install again and is told something that finished ten minutes ago is
        // still in progress, until the next restart.
        let bad = tarball(&[("readme.md", "no manifest")]);
        let http = Arc::new(Canned::new(npm_metadata("sad", "1.0.0", &bad), bad));
        let fx = fixture(http);
        fx.installer
            .start_download(InstallSource::Npm {
                name: "sad".to_string(),
                version: None,
            })
            .unwrap();
        for _ in 0..200 {
            if !fx.events.done.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let done = fx.events.done.lock().unwrap().clone();
        assert_eq!(done.len(), 1);
        assert!(!done[0].2, "a failed job reports ok=false");
        assert_eq!(done[0].3, 1);
        assert!(!fx.installer.is_busy("sad"));
    }

    #[tokio::test]
    async fn a_second_job_for_the_same_extension_is_refused_while_the_first_runs() {
        let fx = fixture(Arc::new(NoNetwork));
        let claim = fx
            .installer
            .inner
            .inflight
            .clone()
            .claim_owned("busy")
            .unwrap();
        let err = fx.installer.start_install_deps("busy").unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        drop(claim);
        // And it is only the claim in the way: with it gone the same call is
        // accepted, which is what makes the refusal above about concurrency
        // rather than about the extension.
        assert!(fx.installer.start_install_deps("busy").is_ok());
    }

    /// A `deno` that exits 0, so the install phase has something to spawn.
    ///
    /// Without it the update test can only use a DEPENDENCY-FREE extension,
    /// which `finish_register` already marks `installed` during the download
    /// phase, so deleting the second half of the update changes nothing an
    /// assertion can see. That is a test that cannot fail, and a mutation
    /// found it.
    #[cfg(unix)]
    fn plant_fake_deno(paths: &Paths) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(paths.bin_dir()).unwrap();
        let path = paths.bin_path("deno");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_update_runs_both_phases_under_one_job_id() {
        // Two job ids would make the client show a completed install halfway
        // through, and the second half would then re-pin under an id nothing
        // was listening for.
        //
        // The extension DECLARES dependencies, which is what makes the second
        // phase observable: the download alone leaves it at `downloaded`, and
        // only the install phase resolves the imports and pins the hash.
        let bytes = tarball(&[
            ("loca.json", &manifest_json("upd", one_tool())),
            ("deno.json", r#"{"imports":{"zod":"npm:zod@3"}}"#),
        ]);
        let http = Arc::new(Canned::new(
            npm_metadata("upd", "2.0.0", &bytes),
            bytes.clone(),
        ));
        let fx = fixture(http);
        plant_fake_deno(&fx.paths);
        let started = fx
            .installer
            .start_update(InstallSource::Npm {
                name: "upd".to_string(),
                version: None,
            })
            .unwrap();
        for _ in 0..400 {
            if !fx.events.done.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let done = fx.events.done.lock().unwrap().clone();
        assert_eq!(done.len(), 1, "one job, so one done frame");
        assert_eq!(done[0].0, started.job_id);
        assert!(done[0].2, "the update failed: {done:?}");
        let row = fx.installer.registry().get_or_err("upd").await.unwrap();
        assert!(
            row.has_deps,
            "the fixture must declare deps or this test cannot fail"
        );
        assert_eq!(
            row.status,
            ExtensionStatus::Installed,
            "an update must land back at installed, or the next verify calls it drift"
        );
        assert!(
            !row.content_hash.is_empty(),
            "the install phase is what pins the hash"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dependency_bearing_install_spawns_deno_and_then_pins() {
        let fx = fixture(Arc::new(NoNetwork));
        plant_fake_deno(&fx.paths);
        let folder = fx.paths.extension_dir("withdeps");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("loca.json"),
            manifest_json("withdeps", one_tool()),
        )
        .unwrap();
        std::fs::write(
            folder.join("deno.json"),
            r#"{"imports":{"z":"jsr:@std/path"}}"#,
        )
        .unwrap();
        fx.installer
            .register_local_downloaded("withdeps")
            .await
            .unwrap();
        assert_eq!(
            fx.installer
                .registry()
                .get_or_err("withdeps")
                .await
                .unwrap()
                .status,
            ExtensionStatus::Downloaded,
            "a deps-bearing extension waits for the explicit Install step"
        );

        fx.installer
            .run_install_deps("withdeps", "job-1")
            .await
            .unwrap();
        let row = fx
            .installer
            .registry()
            .get_or_err("withdeps")
            .await
            .unwrap();
        assert_eq!(row.status, ExtensionStatus::Installed);
        assert!(!row.content_hash.is_empty());
    }

    #[tokio::test]
    async fn the_npm_resolution_is_logged_under_the_job_that_asked_for_it() {
        let bytes = tarball(&[("loca.json", &manifest_json("logged", one_tool()))]);
        let http = Arc::new(Canned::new(
            npm_metadata("logged", "4.5.6", &bytes),
            bytes.clone(),
        ));
        let fx = fixture(http);
        fx.installer
            .run_download(
                InstallSource::Npm {
                    name: "logged".to_string(),
                    version: None,
                },
                "logged",
                "job-7",
            )
            .await
            .unwrap();
        let logs = fx.events.logs.lock().unwrap().clone();
        assert!(
            logs.iter().any(|(job, stream, line)| job == "job-7"
                && stream == "stdout"
                && line.contains("logged@4.5.6")),
            "expected a resolution line, got {logs:?}"
        );
    }

    // --- the dev channel ---------------------------------------------------

    #[tokio::test]
    async fn the_dev_channel_installs_a_seeded_extension_from_the_checkout_with_no_network() {
        let fx = fixture_on(Arc::new(NoNetwork), Channel::Dev);
        // The built-in lives in the repo next to this crate. If the checkout is
        // not there the dev path has nothing to prove, so the test skips rather
        // than asserting about someone else's directory layout.
        let source = super::codebase_path("loca-extension-builtin");
        if !source.join("loca.json").exists() {
            return;
        }
        fx.installer
            .run_download(
                InstallSource::Seeded {
                    id: seed::BUILTIN_EXTENSION_ID.to_string(),
                    planted: None,
                },
                seed::BUILTIN_EXTENSION_ID,
                "job-1",
            )
            .await
            .unwrap();
        let row = fx
            .installer
            .registry()
            .get_or_err(seed::BUILTIN_EXTENSION_ID)
            .await
            .unwrap();
        assert!(
            row.version.contains("+dev."),
            "a checkout install is stamped so an edit reads as a new version, got {}",
            row.version
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod broadcast_tests {
    use super::*;
    use crate::transport::frame_bus::{FrameBus, Outbound};
    use serde_json::json;

    /// Drain `n` frames as JSON.
    ///
    /// BOUNDED, and not as a precaution: a connection with nothing queued parks
    /// forever, so a missing frame here is a HANG rather than a failure, and a
    /// test that hangs is one nobody reads the result of. This was watched
    /// happen: dropping the snapshot from `done` wedged the whole test binary
    /// instead of reporting red.
    async fn kinds(
        conn: &mut crate::transport::frame_bus::Connection,
        n: usize,
    ) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        for i in 0..n {
            let item = tokio::time::timeout(std::time::Duration::from_secs(5), conn.next())
                .await
                .unwrap_or_else(|_| panic!("frame {} of {n} never arrived", i + 1));
            let Some(Outbound::Frame(json)) = item else {
                panic!("expected a frame, got {item:?}");
            };
            out.push(serde_json::from_str(&json).unwrap());
        }
        out
    }

    /// Mutation: drop the `ExtensionSnapshot` from `done`, or address either
    /// frame to one client. Both leave a second paired client watching an
    /// install it can see start and never see finish.
    #[tokio::test]
    async fn an_install_streams_to_every_paired_client_and_ends_with_a_snapshot() {
        let bus = Arc::new(FrameBus::new());
        let mut watcher = bus.register("the-other-client");
        let events = BroadcastInstallEvents::new(Arc::clone(&bus));

        events.log("job-1", "acme", ServerToClientFrameStream::Stderr, "step");
        events.done("job-1", "acme", true, 0);

        assert_eq!(
            kinds(&mut watcher, 3).await,
            vec![
                json!({
                    "kind": "extension.install_log",
                    "id": "acme",
                    "jobId": "job-1",
                    "line": "step",
                    "stream": "stderr",
                }),
                json!({
                    "kind": "extension.install_done",
                    "id": "acme",
                    "jobId": "job-1",
                    "ok": true,
                    "code": 0,
                }),
                json!({ "kind": "extension.snapshot" }),
            ],
        );
    }
}
