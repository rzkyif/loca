//! First-boot seeding of the extensions Loca ships.
//!
//! # The rule this module exists to obey
//!
//! Boot is not a user action, so **first-boot seeding never fetches**. The
//! installer plants a verified tarball and its signed manifest during the
//! install phase, where network is expected; boot re-verifies those bytes
//! offline and installs from them, or defers to the user-gated install.
//!
//! That is why [`plan_seed`] is a pure function returning a [`SeedAction`]
//! rather than a procedure that fetches when it runs out of local options: the
//! test for the rule is then structural rather than observational.
//!
//! A seeded extension lands at `downloaded`, because installing its
//! dependencies needs the `deno` runtime and `deno install` is itself a network
//! action. Install and per-tool enable stay explicit user steps.

use std::path::{Path, PathBuf};

use crate::config::Channel;

use super::manifest::{read_planted, SeededManifest};
use super::runner::{Installer, OnSuccess};
use super::{InstallSource, Planted};

/// One extension Loca ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeededExtension {
    /// Also the folder name under the extensions directory.
    pub id: &'static str,
    /// The `packages/` directory that is both its release source and, in dev,
    /// its byte source.
    pub dir: &'static str,
    /// Never released, never planted, never installable outside dev.
    pub dev_only: bool,
}

pub const BUILTIN_EXTENSION_ID: &str = "loca-extension-builtin";
pub const SAMPLES_EXTENSION_ID: &str = "loca-extension-samples";

pub const SEEDED_EXTENSIONS: &[SeededExtension] = &[
    SeededExtension {
        id: BUILTIN_EXTENSION_ID,
        dir: "loca-extension-builtin",
        dev_only: false,
    },
    SeededExtension {
        id: SAMPLES_EXTENSION_ID,
        dir: "loca-extension-samples",
        dev_only: true,
    },
];

pub fn seeded_by_id(id: &str) -> Option<&'static SeededExtension> {
    SEEDED_EXTENSIONS.iter().find(|ext| ext.id == id)
}

/// The installer-planted artefact names, read on first boot for an offline
/// install. A contract with the native installers and the release payload
/// staging, which write them.
pub fn planted_tarball_name(id: &str) -> String {
    format!(".{id}.tgz")
}

pub fn planted_manifest_name(id: &str) -> String {
    format!(".{id}.json")
}

/// What boot found out about one seeded extension. Passed in rather than
/// discovered inside [`plan_seed`], so the decision is a pure function over
/// stated facts.
#[derive(Debug, Clone)]
pub struct SeedFacts {
    /// The per-extension marker file exists: this extension has been seeded
    /// once already, so a user who later deleted it must not get it back.
    pub marked: bool,
    /// A registry row exists, from a prior boot or a manual install.
    pub registered: bool,
    pub channel: Channel,
    /// The verified planted manifest, when there is one.
    pub planted: Option<SeededManifest>,
    /// The planted tarball is present on disk.
    pub tarball_present: bool,
}

/// What boot should do about one seeded extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedAction {
    /// Nothing at all: it is `dev_only` off the dev channel.
    SkipDevOnly,
    /// Already handled. Nothing to do.
    AlreadySeeded,
    /// Present already, so only the marker is missing: record it, so a later
    /// user delete is not undone on the next boot.
    RecordOnly,
    /// Dev: resolve the bytes from the codebase. No plant, no network.
    InstallFromCodebase,
    /// Production: install offline from these planted artefacts.
    InstallFromPlant {
        tarball: PathBuf,
        manifest: Box<SeededManifest>,
    },
    /// Not planted. Log and leave it to the user-gated install.
    ///
    /// **This is the arm that must never become a fetch.** Every other outcome
    /// is local, so this is the only place a "just download it" would fit.
    DeferToUser,
}

/// Decide what to do about one seeded extension, without touching anything.
pub fn plan_seed(ext: &SeededExtension, extensions_dir: &Path, facts: &SeedFacts) -> SeedAction {
    // Before every other consideration, so no later branch can reach a state
    // where a dev-only extension installs in production.
    if ext.dev_only && facts.channel != Channel::Dev {
        return SeedAction::SkipDevOnly;
    }
    if facts.marked {
        return SeedAction::AlreadySeeded;
    }
    if facts.registered {
        return SeedAction::RecordOnly;
    }
    if facts.channel == Channel::Dev {
        return SeedAction::InstallFromCodebase;
    }
    match (&facts.planted, facts.tarball_present) {
        // BOTH halves: the manifest is only the anchor, and the bytes it
        // anchors have to exist.
        (Some(manifest), true) => SeedAction::InstallFromPlant {
            tarball: extensions_dir.join(planted_tarball_name(ext.id)),
            manifest: Box::new(manifest.clone()),
        },
        _ => SeedAction::DeferToUser,
    }
}

/// Read the facts [`plan_seed`] needs, from disk only. [`read_planted`]
/// verifies the Ed25519 signature offline, so an unverifiable plant reaches the
/// planner as no plant at all.
pub async fn read_seed_facts(
    ext: &SeededExtension,
    extensions_dir: &Path,
    marker: &Path,
    registered: bool,
    channel: Channel,
) -> SeedFacts {
    let tarball = extensions_dir.join(planted_tarball_name(ext.id));
    SeedFacts {
        marked: tokio::fs::metadata(marker).await.is_ok(),
        registered,
        channel,
        planted: read_planted(&extensions_dir.join(planted_manifest_name(ext.id))).await,
        tarball_present: tokio::fs::metadata(&tarball).await.is_ok(),
    }
}

/// Record that an extension has been seeded. Best-effort: a failure means it is
/// re-seeded next boot, which is wasteful but not wrong.
pub async fn record_seeded(marker: &Path) {
    if let Some(parent) = marker.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(err) = tokio::fs::write(marker, b"").await {
        tracing::warn!(
            target: "loca::extensions",
            "failed to write seed marker {}: {err}", marker.display(),
        );
    }
}

/// Seed every extension Loca ships: the built-in everywhere, plus the dev-only
/// showcase on the dev channel. Each lands at status `downloaded`.
///
/// Boot's single call. It awaits the DECISION only and hands the install to a
/// background job, so boot is not held open behind a copy or an extract. It
/// cannot fail: a per-extension failure is logged and the next boot retries.
pub async fn seed_extensions_if_needed(installer: &Installer) {
    for ext in SEEDED_EXTENSIONS {
        // `plan_seed` checks this again; here it keeps production from even
        // reading the disk for an extension it will never install.
        if ext.dev_only && installer.channel() != Channel::Dev {
            continue;
        }
        seed_one(installer, ext).await;
    }
}

async fn seed_one(installer: &Installer, ext: &'static SeededExtension) {
    let extensions_dir = installer.paths().extensions_dir();
    let marker = installer.paths().seeded_marker_file(ext.id);
    let registered = match installer.registry().get(ext.id).await {
        Ok(row) => row.is_some(),
        Err(err) => {
            // An unreadable registry cannot say whether the user deleted this
            // extension, and re-seeding a deleted one has no way back.
            tracing::warn!(
                target: "loca::extensions",
                "{} seed skipped: {}", ext.id, err.message,
            );
            return;
        }
    };
    let facts = read_seed_facts(
        ext,
        &extensions_dir,
        &marker,
        registered,
        installer.channel(),
    )
    .await;
    let action = plan_seed(ext, &extensions_dir, &facts);

    if let Some(spec) = seed_install_source(ext, &action) {
        // The marker flips on SUCCESS only, so a failed first boot retries and a
        // deliberate delete is never undone.
        let hook: OnSuccess =
            Box::new(move || Box::pin(async move { record_seeded(&marker).await }));
        if let Err(err) = installer.start_download_then(spec, hook) {
            // A user-triggered install or update already holds this extension's
            // claim. Let that one win rather than failing boot seeding.
            tracing::warn!(
                target: "loca::extensions",
                "{} seed skipped: {}", ext.id, err.message,
            );
        }
        return;
    }

    match action {
        // Present already, so only the marker is missing; writing it stops a
        // later user delete being undone.
        SeedAction::RecordOnly => record_seeded(&marker).await,
        SeedAction::DeferToUser => tracing::info!(
            target: "loca::extensions",
            "{} not planted for offline seed; deferring to the user-gated install",
            ext.id,
        ),
        SeedAction::SkipDevOnly | SeedAction::AlreadySeeded => {}
        // Unreachable: `seed_install_source` answered `Some` for both.
        SeedAction::InstallFromCodebase | SeedAction::InstallFromPlant { .. } => {}
    }
}

/// The install spec one [`SeedAction`] resolves to, or `None` when boot must
/// install nothing. Pure and separate from the executor, because
/// `Seeded { planted: None }` resolves from the CODEBASE on dev and from the
/// signed CDN tarball anywhere else, so producing it off dev would be a
/// boot-time fetch. Only [`SeedAction::InstallFromCodebase`] produces it.
fn seed_install_source(ext: &SeededExtension, action: &SeedAction) -> Option<InstallSource> {
    match action {
        SeedAction::InstallFromCodebase => Some(InstallSource::Seeded {
            id: ext.id.to_string(),
            planted: None,
        }),
        SeedAction::InstallFromPlant { tarball, manifest } => Some(InstallSource::Seeded {
            id: ext.id.to_string(),
            planted: Some(Box::new(Planted {
                tarball_path: tarball.clone(),
                manifest: (**manifest).clone(),
            })),
        }),
        SeedAction::SkipDevOnly
        | SeedAction::AlreadySeeded
        | SeedAction::RecordOnly
        | SeedAction::DeferToUser => None,
    }
}

/// May this seeded extension be installed on this channel at all? The
/// chokepoint for the HTTP paths as well as boot: a `dev_only` extension is
/// never released and never planted.
pub fn assert_installable(ext: &SeededExtension, channel: Channel) -> crate::error::Result<()> {
    if ext.dev_only && channel != Channel::Dev {
        return Err(crate::error::validation(format!(
            "{} is a dev-only extension and cannot be installed on the {} channel",
            ext.id,
            channel_name(channel),
        )));
    }
    Ok(())
}

fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "stable",
        Channel::Dev => "dev",
        Channel::Latest => "latest",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn manifest(version: &str) -> SeededManifest {
        SeededManifest {
            schema_version: 1,
            version: version.to_string(),
            id: BUILTIN_EXTENSION_ID.to_string(),
            tarball_url: "https://get.loca.rzkyif.com/x.tgz".to_string(),
            sha256: "a".repeat(64),
            signature: "sig".to_string(),
        }
    }

    fn facts(channel: Channel) -> SeedFacts {
        SeedFacts {
            marked: false,
            registered: false,
            channel,
            planted: None,
            tarball_present: false,
        }
    }

    fn builtin() -> &'static SeededExtension {
        seeded_by_id(BUILTIN_EXTENSION_ID).unwrap()
    }

    fn samples() -> &'static SeededExtension {
        seeded_by_id(SAMPLES_EXTENSION_ID).unwrap()
    }

    const DIR: &str = "/home/u/.loca/stable/core/extensions";

    /// Every (extension, facts) pair the planner can be handed.
    fn every_case() -> Vec<(&'static SeededExtension, SeedFacts)> {
        let mut out = Vec::new();
        for channel in [Channel::Stable, Channel::Dev, Channel::Latest] {
            for marked in [false, true] {
                for registered in [false, true] {
                    for planted in [None, Some(manifest("1.0.0"))] {
                        for tarball_present in [false, true] {
                            for ext in [builtin(), samples()] {
                                out.push((
                                    ext,
                                    SeedFacts {
                                        marked,
                                        registered,
                                        channel,
                                        planted: planted.clone(),
                                        tarball_present,
                                    },
                                ));
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// **The structural test for the no-network rule.** Observational versions
    /// prove nothing, because the offline path is also what runs when
    /// everything works, so this asserts on the TYPE: every reachable outcome
    /// is one of six local actions, and adding a variant fails this test.
    #[test]
    fn no_reachable_seed_outcome_can_fetch_anything() {
        let mut seen = Vec::new();
        for (ext, facts) in every_case() {
            let name = match plan_seed(ext, Path::new(DIR), &facts) {
                SeedAction::SkipDevOnly => "skip",
                SeedAction::AlreadySeeded => "already",
                SeedAction::RecordOnly => "record",
                SeedAction::InstallFromCodebase => "codebase",
                SeedAction::InstallFromPlant { .. } => "plant",
                SeedAction::DeferToUser => "defer",
            };
            if !seen.contains(&name) {
                seen.push(name);
            }
        }
        seen.sort_unstable();
        assert_eq!(
            seen,
            vec!["already", "codebase", "defer", "plant", "record", "skip"],
        );
    }

    #[test]
    fn a_missing_plant_defers_to_the_user_rather_than_fetching() {
        // The single most important branch in this file.
        assert_eq!(
            plan_seed(builtin(), Path::new(DIR), &facts(Channel::Stable)),
            SeedAction::DeferToUser
        );
        assert_eq!(
            plan_seed(builtin(), Path::new(DIR), &facts(Channel::Latest)),
            SeedAction::DeferToUser
        );
    }

    #[test]
    fn a_manifest_without_its_tarball_is_not_an_install() {
        let planted_only = SeedFacts {
            planted: Some(manifest("1.0.0")),
            tarball_present: false,
            ..facts(Channel::Stable)
        };
        assert_eq!(
            plan_seed(builtin(), Path::new(DIR), &planted_only),
            SeedAction::DeferToUser
        );

        // And a tarball with no verified manifest is bytes nobody signed.
        let tarball_only = SeedFacts {
            planted: None,
            tarball_present: true,
            ..facts(Channel::Stable)
        };
        assert_eq!(
            plan_seed(builtin(), Path::new(DIR), &tarball_only),
            SeedAction::DeferToUser
        );
    }

    #[test]
    fn a_complete_plant_installs_offline_from_those_exact_bytes() {
        let complete = SeedFacts {
            planted: Some(manifest("2.1.0")),
            tarball_present: true,
            ..facts(Channel::Stable)
        };
        let action = plan_seed(builtin(), Path::new(DIR), &complete);
        match action {
            SeedAction::InstallFromPlant { tarball, manifest } => {
                assert_eq!(
                    tarball,
                    Path::new(DIR).join(".loca-extension-builtin.tgz"),
                    "the planted tarball name must match what the installer wrote"
                );
                assert_eq!(manifest.version, "2.1.0");
            }
            other => panic!("expected an offline install, got {other:?}"),
        }
    }

    #[test]
    fn the_dev_only_extension_can_never_be_seeded_in_production() {
        for channel in [Channel::Stable, Channel::Latest] {
            let complete = SeedFacts {
                planted: Some(manifest("1.0.0")),
                tarball_present: true,
                ..facts(channel)
            };
            assert_eq!(
                plan_seed(samples(), Path::new(DIR), &complete),
                SeedAction::SkipDevOnly,
                "{channel:?}"
            );
            // And the HTTP chokepoint agrees.
            assert!(assert_installable(samples(), channel).is_err());
        }
        assert_eq!(
            plan_seed(samples(), Path::new(DIR), &facts(Channel::Dev)),
            SeedAction::InstallFromCodebase
        );
        assert!(assert_installable(samples(), Channel::Dev).is_ok());
        assert!(assert_installable(builtin(), Channel::Stable).is_ok());
    }

    #[test]
    fn the_dev_only_skip_is_decided_before_anything_else() {
        // Checked after the marker it would answer AlreadySeeded, and after the
        // registry check RecordOnly: wrong answers that look harmless.
        let noisy = SeedFacts {
            marked: true,
            registered: true,
            planted: Some(manifest("1.0.0")),
            tarball_present: true,
            ..facts(Channel::Stable)
        };
        assert_eq!(
            plan_seed(samples(), Path::new(DIR), &noisy),
            SeedAction::SkipDevOnly
        );
    }

    #[test]
    fn a_marker_stops_a_deleted_extension_coming_back_on_the_next_boot() {
        let marked = SeedFacts {
            marked: true,
            planted: Some(manifest("1.0.0")),
            tarball_present: true,
            ..facts(Channel::Stable)
        };
        assert_eq!(
            plan_seed(builtin(), Path::new(DIR), &marked),
            SeedAction::AlreadySeeded
        );
    }

    #[test]
    fn an_already_registered_extension_only_gains_its_marker() {
        // Re-installing would overwrite a folder the user may have updated.
        let registered = SeedFacts {
            registered: true,
            planted: Some(manifest("1.0.0")),
            tarball_present: true,
            ..facts(Channel::Stable)
        };
        assert_eq!(
            plan_seed(builtin(), Path::new(DIR), &registered),
            SeedAction::RecordOnly
        );
    }

    #[test]
    fn dev_resolves_from_the_codebase_and_never_looks_for_a_plant() {
        // On dev the from-source build IS the trust anchor.
        assert_eq!(
            plan_seed(builtin(), Path::new(DIR), &facts(Channel::Dev)),
            SeedAction::InstallFromCodebase
        );
        let with_plant = SeedFacts {
            planted: Some(manifest("9.9.9")),
            tarball_present: true,
            ..facts(Channel::Dev)
        };
        assert_eq!(
            plan_seed(builtin(), Path::new(DIR), &with_plant),
            SeedAction::InstallFromCodebase
        );
    }

    #[test]
    fn the_planted_artefact_names_match_the_planting_side() {
        // A rename silently stops every fresh install from finding its
        // built-in, and the symptom is an empty list rather than an error.
        assert_eq!(
            planted_tarball_name(BUILTIN_EXTENSION_ID),
            ".loca-extension-builtin.tgz"
        );
        assert_eq!(
            planted_manifest_name(BUILTIN_EXTENSION_ID),
            ".loca-extension-builtin.json"
        );
    }

    #[test]
    fn the_seeded_set_is_the_built_in_plus_the_dev_only_showcase() {
        assert_eq!(SEEDED_EXTENSIONS.len(), 2);
        assert!(!builtin().dev_only, "the built-in must ship in production");
        assert!(samples().dev_only, "the samples showcase must never ship");
        assert!(seeded_by_id("something-else").is_none());
    }

    #[tokio::test]
    async fn an_unverifiable_plant_reads_as_no_plant_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let extensions = dir.path().join("extensions");
        tokio::fs::create_dir_all(&extensions).await.unwrap();
        tokio::fs::write(
            extensions.join(planted_manifest_name(BUILTIN_EXTENSION_ID)),
            r#"{"schemaVersion":1,"version":"1.0.0","id":"loca-extension-builtin",
                "tarballUrl":"u","sha256":"a","signature":"AAAA"}"#,
        )
        .await
        .unwrap();
        tokio::fs::write(
            extensions.join(planted_tarball_name(BUILTIN_EXTENSION_ID)),
            b"not really a tarball",
        )
        .await
        .unwrap();

        let facts = read_seed_facts(
            builtin(),
            &extensions,
            &dir.path().join("marker"),
            false,
            Channel::Stable,
        )
        .await;
        assert!(facts.tarball_present, "the fixture did not stage a tarball");
        assert!(
            facts.planted.is_none(),
            "a manifest with a bad signature was accepted"
        );
        assert_eq!(
            plan_seed(builtin(), &extensions, &facts),
            SeedAction::DeferToUser
        );
    }

    #[tokio::test]
    async fn the_marker_is_written_where_a_later_boot_will_look_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("nested").join(".seeded-builtin");
        record_seeded(&marker).await;
        assert!(marker.exists());

        let facts = read_seed_facts(builtin(), dir.path(), &marker, false, Channel::Stable).await;
        assert!(facts.marked);
    }

    /// **The other half of the no-network rule.** The executor is what turns an
    /// outcome into a spec, and `Seeded { planted: None }` resolves from the
    /// codebase on dev but fetches the signed CDN tarball anywhere else, so the
    /// spec that is offline on one channel is a boot-time fetch on the others.
    #[test]
    fn boot_never_hands_the_installer_a_spec_that_would_fetch() {
        let mut planted_specs = 0;
        let mut codebase_specs = 0;
        for (ext, facts) in every_case() {
            let channel = facts.channel;
            let action = plan_seed(ext, Path::new(DIR), &facts);
            match seed_install_source(ext, &action) {
                None => {}
                Some(InstallSource::Seeded { planted: None, .. }) => {
                    assert_eq!(
                        channel,
                        Channel::Dev,
                        "boot asked for an unplanted seeded install on {channel:?}, which \
                         resolves and fetches the CDN tarball"
                    );
                    codebase_specs += 1;
                }
                Some(InstallSource::Seeded {
                    planted: Some(_), ..
                }) => planted_specs += 1,
                Some(other) => {
                    panic!("boot may only ever install a SEEDED extension, got {other:?}")
                }
            }
        }
        // Both shapes are reachable, so the assertion above is not vacuous.
        assert!(codebase_specs > 0 && planted_specs > 0);
    }

    // --- the executor -----------------------------------------------------

    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;

    use crate::config::Config;
    use crate::contract::api::{ExtensionStatus, ServerToClientFrameStream};
    use crate::db::Db;
    use crate::error::{AppError, ErrorCode, Result};
    use crate::extensions::registry::Registry;
    use crate::paths::Paths;
    use crate::sidecars::Manager;

    use super::super::net::Http;
    use super::super::runner::{InstallEvents, Installer, InstallerConfig, NoMemories};

    /// Records every URL anything asked for, and refuses it. A recorder rather
    /// than the panicking `NoNetwork` double, because a boot-time fetch happens
    /// inside a SPAWNED job where a panic never reaches the test.
    #[derive(Default)]
    struct RecordingHttp {
        urls: Mutex<Vec<String>>,
    }

    impl RecordingHttp {
        fn urls(&self) -> Vec<String> {
            self.urls.lock().unwrap().clone()
        }
        fn record(&self, url: &str) -> AppError {
            self.urls.lock().unwrap().push(url.to_string());
            AppError::new(ErrorCode::ServerUnavailable, format!("refused {url}"))
        }
    }

    #[async_trait]
    impl Http for RecordingHttp {
        async fn get_text(&self, url: &str) -> Result<String> {
            Err(self.record(url))
        }
        async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
            Err(self.record(url))
        }
    }

    /// Every job outcome, so a test can tell "nothing was started" from "a job
    /// ran and failed".
    #[derive(Default)]
    struct Outcomes {
        done: Mutex<Vec<(String, bool)>>,
    }

    impl Outcomes {
        fn all(&self) -> Vec<(String, bool)> {
            self.done.lock().unwrap().clone()
        }
    }

    impl InstallEvents for Outcomes {
        fn log(&self, _: &str, _: &str, _: ServerToClientFrameStream, _: &str) {}
        fn done(&self, _job_id: &str, extension_id: &str, ok: bool, _code: i64) {
            self.done
                .lock()
                .unwrap()
                .push((extension_id.to_string(), ok));
        }
    }

    struct Boot {
        installer: Installer,
        paths: Paths,
        http: Arc<RecordingHttp>,
        outcomes: Arc<Outcomes>,
        _dir: tempfile::TempDir,
    }

    impl Boot {
        fn new(channel: Channel) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::rooted(dir.path());
            std::fs::create_dir_all(paths.extensions_dir()).unwrap();
            let db = Db::open_in_memory().unwrap();
            db.call_blocking(|conn| crate::db::migrate(conn)).unwrap();
            let http = Arc::new(RecordingHttp::default());
            let outcomes = Arc::new(Outcomes::default());
            let settings = crate::settings::Settings::new(
                &paths,
                db.clone(),
                Arc::new(crate::transport::FrameBus::new()),
            );
            let installer = Installer::new(InstallerConfig {
                paths: paths.clone(),
                config: Config {
                    channel,
                    host: "127.0.0.1".to_string(),
                    port: 1,
                    version: "0.0.0",
                    storage_base_url: "https://get.example.test".to_string(),
                    hf_base_url: "https://hf.example.test".to_string(),
                },
                settings,
                registry: Registry::new(db, paths.clone()),
                http: http.clone(),
                sidecars: Arc::new(Manager::new()),
                memories: Arc::new(NoMemories),
                events: outcomes.clone(),
            });
            Self {
                installer,
                paths,
                http,
                outcomes,
                _dir: dir,
            }
        }

        /// Run boot seeding and wait for anything it started to finish. A job
        /// claims its extension SYNCHRONOUSLY before `start` returns, so "no
        /// seeded id is busy on return" means no job was started at all.
        async fn seed(&self) {
            seed_extensions_if_needed(&self.installer).await;
            for _ in 0..2_000 {
                if !SEEDED_EXTENSIONS
                    .iter()
                    .any(|ext| self.installer.is_busy(ext.id))
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("a seed job never finished");
        }

        fn marked(&self, id: &str) -> bool {
            self.paths.seeded_marker_file(id).exists()
        }

        async fn registered(&self, id: &str) -> bool {
            self.installer.registry().get(id).await.unwrap().is_some()
        }
    }

    #[tokio::test]
    async fn a_production_boot_with_no_plant_starts_nothing_and_asks_for_nothing() {
        // The way this goes wrong is a well-meaning "it is missing, so download
        // it".
        let boot = Boot::new(Channel::Stable);
        boot.seed().await;

        assert_eq!(
            boot.http.urls(),
            Vec::<String>::new(),
            "boot reached the network"
        );
        assert_eq!(boot.outcomes.all(), vec![], "boot started an install job");
        assert!(!boot.registered(BUILTIN_EXTENSION_ID).await);
        assert!(
            !boot.marked(BUILTIN_EXTENSION_ID),
            "a deferral must not be recorded as a seed, or the plant that \
             arrives with the next update would never be installed"
        );
    }

    #[tokio::test]
    async fn the_dev_only_showcase_is_not_even_looked_at_in_production() {
        let boot = Boot::new(Channel::Latest);
        boot.seed().await;
        assert!(!boot.registered(SAMPLES_EXTENSION_ID).await);
        assert!(!boot.marked(SAMPLES_EXTENSION_ID));
    }

    #[tokio::test]
    async fn a_dev_boot_seeds_both_shipped_extensions_from_the_codebase_offline() {
        // The positive control for every negative assertion above: two rows at
        // `downloaded`, two markers, and no request.
        let boot = Boot::new(Channel::Dev);
        boot.seed().await;

        assert_eq!(
            boot.http.urls(),
            Vec::<String>::new(),
            "the dev seed reached the network; it resolves from the checkout"
        );
        let mut done = boot.outcomes.all();
        done.sort();
        assert_eq!(
            done,
            vec![
                (BUILTIN_EXTENSION_ID.to_string(), true),
                (SAMPLES_EXTENSION_ID.to_string(), true),
            ]
        );
        for id in [BUILTIN_EXTENSION_ID, SAMPLES_EXTENSION_ID] {
            let row = boot.installer.registry().get_or_err(id).await.unwrap();
            // Never `installed`: the deno runtime may not be downloaded yet,
            // and Install plus per-tool Enable are explicit user steps.
            assert_eq!(row.status, ExtensionStatus::Downloaded, "{id}");
            assert!(boot.marked(id), "{id} was seeded without recording it");
        }
    }

    #[tokio::test]
    async fn the_second_boot_seeds_nothing_and_the_third_boot_after_a_delete_still_does_not() {
        let boot = Boot::new(Channel::Dev);
        boot.seed().await;
        assert_eq!(boot.outcomes.all().len(), 2);

        // The marker short-circuits before the registry is even consulted.
        boot.seed().await;
        assert_eq!(
            boot.outcomes.all().len(),
            2,
            "a second boot re-ran the install"
        );

        // The user deletes the built-in: the row goes and the marker stays,
        // which is what stops the next restart putting it back.
        boot.installer
            .registry()
            .delete(BUILTIN_EXTENSION_ID)
            .await
            .unwrap();
        boot.seed().await;
        assert_eq!(
            boot.outcomes.all().len(),
            2,
            "the built-in the user deleted came back on the next boot"
        );
        assert!(!boot.registered(BUILTIN_EXTENSION_ID).await);
    }

    #[tokio::test]
    async fn an_extension_already_present_gains_its_marker_without_being_reinstalled() {
        // Copying over it would discard whatever is there now.
        let boot = Boot::new(Channel::Dev);
        boot.seed().await;
        for id in [BUILTIN_EXTENSION_ID, SAMPLES_EXTENSION_ID] {
            std::fs::remove_file(boot.paths.seeded_marker_file(id)).unwrap();
        }

        boot.seed().await;
        assert_eq!(
            boot.outcomes.all().len(),
            2,
            "a present extension was reinstalled instead of just recorded"
        );
        assert!(boot.marked(BUILTIN_EXTENSION_ID));
        assert!(boot.marked(SAMPLES_EXTENSION_ID));
    }

    #[tokio::test]
    async fn the_recorder_sees_a_request_that_is_supposed_to_happen() {
        // A recorder whose recording was deleted reads empty and leaves every
        // "no request" test above green. This one goes red instead.
        let boot = Boot::new(Channel::Stable);
        boot.installer
            .start_download(InstallSource::Npm {
                name: "some-extension".to_string(),
                version: None,
            })
            .unwrap();
        for _ in 0..2_000 {
            if !boot.installer.is_busy("some-extension") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            !boot.http.urls().is_empty(),
            "a user-triggered npm download made no request the recorder could see"
        );
    }
}
