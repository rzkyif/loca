//! First-boot seeding of the extensions Loca ships.
//!
//! Ports `packages/loca-core/src/extensions/seeding.ts`.
//!
//! # The rule this module exists to obey
//!
//! `AGENTS.md`: once installed, the running Core makes NO outbound network
//! request without an explicit user action. Boot is not a user action, so
//! **first-boot seeding never fetches**. The installer plants a verified
//! tarball and its signed manifest under the extensions directory during the
//! install phase, where network is expected; boot re-verifies those bytes
//! offline and installs from them. If the plant is missing, boot logs and
//! defers to the user-gated install in the extensions UI.
//!
//! That is why [`plan_seed`] is a pure function returning a [`SeedAction`]
//! rather than a procedure that fetches when it runs out of local options.
//! There is no branch it can take that reaches the network, and the test for
//! that is structural rather than observational: a test that runs the offline
//! path and sees no error proves nothing, because the offline path is also what
//! runs when everything works.
//!
//! # A seeded extension lands at `downloaded`, on purpose
//!
//! Installing its dependencies needs the `deno` worker runtime, which may not
//! be downloaded yet this early, and `deno install` is itself a network action.
//! Install and per-tool enable stay explicit user steps, exactly as for any
//! other extension.

use std::path::{Path, PathBuf};

use crate::config::Channel;

use super::manifest::{read_planted, SeededManifest};
use super::runner::{Installer, OnSuccess};
use super::{InstallSource, Planted};

/// One extension Loca ships.
///
/// TODO(F2): `SEEDED_EXTENSIONS` (`domain/extension.ts:37`) has no generated
/// Rust counterpart, so this is the interim declaration and the single place it
/// is written down. When the contract emits it, this constant goes and the
/// imports change; nothing else in this module reads the fields directly.
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
/// install. Kept in step with the planting sides: `install/bootstrap.ts` and
/// the release payload staging in `scripts/release/unified-installers.ts`.
pub fn planted_tarball_name(id: &str) -> String {
    format!(".{id}.tgz")
}

pub fn planted_manifest_name(id: &str) -> String {
    format!(".{id}.json")
}

/// What boot found out about one seeded extension.
///
/// Passed in rather than discovered inside [`plan_seed`], so the decision is a
/// pure function over stated facts and every branch is testable without a
/// filesystem, a database, or a clock.
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
    /// is a local one, so this is the only place a "just download it" would fit
    /// and the only place a port could break the rule the product is built on.
    DeferToUser,
}

/// Decide what to do about one seeded extension, without touching anything.
pub fn plan_seed(ext: &SeededExtension, extensions_dir: &Path, facts: &SeedFacts) -> SeedAction {
    // A dev-only extension is skipped before every other consideration, so no
    // later branch can reach a state where samples installs in production.
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
        // BOTH halves. A manifest with no tarball is not an install: the
        // manifest is only the anchor, and the bytes it anchors have to exist.
        (Some(manifest), true) => SeedAction::InstallFromPlant {
            tarball: extensions_dir.join(planted_tarball_name(ext.id)),
            manifest: Box::new(manifest.clone()),
        },
        _ => SeedAction::DeferToUser,
    }
}

/// Read the facts [`plan_seed`] needs, from disk only.
///
/// The manifest read verifies its Ed25519 signature offline
/// ([`read_planted`]), so an unverifiable plant reaches the planner as no plant
/// at all and lands on [`SeedAction::DeferToUser`].
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

/// Record that an extension has been seeded.
///
/// Best-effort: a failure here means the extension is re-seeded next boot,
/// which is wasteful but not wrong, and failing boot over it would be.
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
/// Boot's single call, and the only production caller of the decision functions
/// above. It awaits the DECISION only (a few stats and one registry read per
/// extension) and hands the install to the installer's own background job, so it
/// does not hold boot open behind a copy or an extract. That matches the oracle,
/// whose `startDownload` is fire-and-forget too (`seeding.ts:122`).
///
/// It cannot fail: a per-extension failure is logged and the next boot retries.
/// Failing boot over a seed would turn "the built-in could not be copied" into
/// "the core does not start".
pub async fn seed_extensions_if_needed(installer: &Installer) {
    for ext in SEEDED_EXTENSIONS {
        // Before every other consideration, so that no later branch can reach a
        // state where the showcase is seeded in production. `plan_seed` checks
        // it again; this one keeps production from even reading the disk for an
        // extension it will never install.
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
            // A registry that cannot be read cannot say whether the user already
            // deleted this extension, and re-seeding a deleted one is the
            // failure with no way back. Leave it for the next boot.
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
        // Present already, from a prior boot or a manual install. Only the
        // marker is missing, and writing it is what stops a later user delete
        // being undone on the boot after that.
        SeedAction::RecordOnly => record_seeded(&marker).await,
        SeedAction::DeferToUser => tracing::info!(
            target: "loca::extensions",
            "{} not planted for offline seed; deferring to the user-gated install",
            ext.id,
        ),
        // Dev-only off dev, and already seeded (or deliberately deleted). Boot
        // has nothing to do and must not say anything about it.
        SeedAction::SkipDevOnly | SeedAction::AlreadySeeded => {}
        // Unreachable: `seed_install_source` answered `Some` for both.
        SeedAction::InstallFromCodebase | SeedAction::InstallFromPlant { .. } => {}
    }
}

/// The install spec one [`SeedAction`] resolves to, or `None` when boot must
/// install nothing.
///
/// Pure, and separate from the executor, because this is where the no-network
/// rule is expressible as a property rather than as a hope. `Seeded { planted:
/// None }` resolves its bytes from the CODEBASE on the dev channel and from the
/// signed CDN tarball on any other ([`super::runner::Installer::run_download`]),
/// so handing the installer that spec off dev would be a boot-time fetch. Only
/// [`SeedAction::InstallFromCodebase`] produces it, and [`plan_seed`] only
/// produces that on dev.
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

/// May this seeded extension be installed on this channel at all?
///
/// The chokepoint for the HTTP paths as well as boot: a `dev_only` extension is
/// never released and never planted, so a user-triggered download of one
/// outside dev could only resolve to something unintended.
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

    /// **The structural test for the no-network rule.**
    ///
    /// Observational versions of this prove nothing: run the offline path, see
    /// no error, and you have watched the path that also runs when everything
    /// works. So this asserts on the TYPE instead. Every reachable outcome of
    /// the planner is one of six local actions, and the one that stands where a
    /// fetch would go is named `DeferToUser`. For boot to reach the network,
    /// somebody would have to add a variant, and this test enumerates them.
    #[test]
    fn no_reachable_seed_outcome_can_fetch_anything() {
        let dir = Path::new(DIR);
        let mut seen = Vec::new();
        for channel in [Channel::Stable, Channel::Dev, Channel::Latest] {
            for marked in [false, true] {
                for registered in [false, true] {
                    for planted in [None, Some(manifest("1.0.0"))] {
                        for tarball_present in [false, true] {
                            for ext in [builtin(), samples()] {
                                let action = plan_seed(
                                    ext,
                                    dir,
                                    &SeedFacts {
                                        marked,
                                        registered,
                                        channel,
                                        planted: planted.clone(),
                                        tarball_present,
                                    },
                                );
                                let name = match action {
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
                        }
                    }
                }
            }
        }
        seen.sort_unstable();
        // If a variant is added, this fails, and whoever added it has to say
        // out loud whether their new outcome reaches the network.
        assert_eq!(
            seen,
            vec!["already", "codebase", "defer", "plant", "record", "skip"],
        );
    }

    #[test]
    fn a_missing_plant_defers_to_the_user_rather_than_fetching() {
        // The single most important branch in this file. In production, with no
        // planted artefacts, boot's answer is "leave it for the user", never
        // "download it".
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
        // The manifest is only the anchor; the bytes it anchors have to be
        // there. Installing on the manifest alone would mean the tarball came
        // from somewhere else, which is the whole thing the signature prevents.
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
        // It is never released and never planted, so anything that DID install
        // under its id came from somewhere unintended.
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
            // And the HTTP chokepoint agrees, so a user-triggered download
            // cannot do what boot refuses to.
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
        // Order matters: checked after the marker, a dev-only extension with a
        // marker file would answer AlreadySeeded, and after the registry check
        // it would answer RecordOnly. Both are wrong answers that look
        // harmless, and both would let a later refactor reach an install.
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
        // Without it, "delete the built-in" is undone by the next restart and
        // the user cannot make it stick.
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
        // Downloaded by a prior boot or installed by hand. Re-installing it
        // would overwrite a folder the user may have updated; the marker is the
        // only thing missing.
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
        // The dev channel has no planted artefacts and no published manifest;
        // the from-source build IS the trust anchor there.
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
        // These strings are a contract with the native installers and the
        // release payload staging. A rename here silently stops every fresh
        // install from finding its built-in, and the symptom is an empty
        // extensions list rather than an error.
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
        // `read_planted` verifies the signature offline and answers None for
        // absent, malformed and tampered alike, so a tampered plant lands on
        // DeferToUser rather than on an install.
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

    /// **The other half of the no-network rule, and the one a refactor can
    /// break.**
    ///
    /// [`plan_seed`] having no fetching outcome is not enough on its own,
    /// because the executor is what turns an outcome into an install spec and
    /// `Seeded { planted: None }` means two different things: on dev it resolves
    /// from the codebase, and on any other channel it resolves and fetches the
    /// signed CDN tarball. So the spec that is offline on one channel is a
    /// boot-time fetch on the others, and this enumerates every reachable
    /// (channel, facts) pair to prove boot never produces it off dev.
    #[test]
    fn boot_never_hands_the_installer_a_spec_that_would_fetch() {
        let dir = Path::new(DIR);
        let mut planted_specs = 0;
        let mut codebase_specs = 0;
        for channel in [Channel::Stable, Channel::Dev, Channel::Latest] {
            for marked in [false, true] {
                for registered in [false, true] {
                    for planted in [None, Some(manifest("1.0.0"))] {
                        for tarball_present in [false, true] {
                            for ext in [builtin(), samples()] {
                                let facts = SeedFacts {
                                    marked,
                                    registered,
                                    channel,
                                    planted: planted.clone(),
                                    tarball_present,
                                };
                                let action = plan_seed(ext, dir, &facts);
                                match seed_install_source(ext, &action) {
                                    None => {}
                                    Some(InstallSource::Seeded { planted: None, .. }) => {
                                        assert_eq!(
                                            channel,
                                            Channel::Dev,
                                            "boot asked for an unplanted seeded install on \
                                             {channel:?}, which resolves and fetches the CDN \
                                             tarball. That is a boot-time network request."
                                        );
                                        codebase_specs += 1;
                                    }
                                    Some(InstallSource::Seeded {
                                        planted: Some(_), ..
                                    }) => planted_specs += 1,
                                    Some(other) => panic!(
                                        "boot may only ever install a SEEDED extension, got \
                                         {other:?}"
                                    ),
                                }
                            }
                        }
                    }
                }
            }
        }
        // Both install shapes are reachable, so the assertion above is not
        // passing because nothing ever produced a spec at all.
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

    /// Records every URL anything asked for, and refuses it.
    ///
    /// A recorder rather than the panicking `NoNetwork` double, because the
    /// request a boot-time fetch would make happens inside a SPAWNED job, where
    /// a panic is captured by the join handle and never reaches the test. The
    /// usual objection to a counter (D-050 case 3: one that never counts always
    /// reads zero) is answered by
    /// `the_recorder_sees_a_request_that_is_supposed_to_happen`, which fails if
    /// the recording is removed.
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

        /// Run boot seeding and wait for anything it started to finish.
        ///
        /// Quiescence is a real signal rather than a guess: a job claims its
        /// extension SYNCHRONOUSLY before `start` returns and releases the claim
        /// only after its success hook and its `done`. So "no seeded id is busy
        /// the moment the executor returns" means no job was started at all.
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
        // The branch the whole module exists for. With no planted artefacts the
        // answer is "leave it to the user-gated install", and the way that goes
        // wrong is a well-meaning "it is missing, so download it".
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
        // The positive control for every negative assertion above, and the
        // conformance suite's `dev` core in miniature: two rows at `downloaded`,
        // two markers, and no request.
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
            // `downloaded`, never `installed`: installing the dependencies needs
            // the deno worker runtime, which may not be downloaded this early,
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

        // Boot again with everything in place: the marker short-circuits before
        // the registry is even consulted.
        boot.seed().await;
        assert_eq!(
            boot.outcomes.all().len(),
            2,
            "a second boot re-ran the install"
        );

        // Now the user deletes the built-in. The row goes and the marker stays,
        // and THAT is what stops the next restart putting it back. Without it
        // the user cannot make a delete stick, and there is no setting to turn
        // it off.
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
        // A prior boot downloaded it, or the user installed it by hand. Copying
        // over it would discard whatever is there now; the marker is the only
        // thing missing.
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
        // Answers the standing objection to any counter-based consent assertion
        // (D-050 case 3): a recorder whose recording was deleted reads empty and
        // leaves every "no request" test above green. This one goes red instead.
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
