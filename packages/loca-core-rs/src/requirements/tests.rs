//! D8's suite.
//!
//! Every test here was watched failing against a named, staged mutation before
//! it was trusted; the mutation each one catches is recorded in
//! `docs/rust-port/packages/D8/status.md` rather than repeated per test.
//!
//! Three rules this file follows, each of which has cost this fleet a green run
//! that meant nothing:
//!
//! - **Every wait is bounded.** A frame that never arrives must FAIL, not hang:
//!   an unbounded drain wedges the whole `--lib` binary and masks every other
//!   result (D-070). [`frames_within`] is the only way this file reads a
//!   broadcast.
//! - **An assertion that something did NOT happen is paired** with one proving
//!   the instrument would have noticed (D-050). Every "no network" assertion
//!   here has a neighbour that counts a real call through the same counter.
//! - **A route test must not build the wiring it is checking.** These mount the
//!   slice through [`routes::routes`], which is the same function the router
//!   calls; the MOUNT itself is orchestrator-owned and unproven here by
//!   construction, and the conformance cases are what cover it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::Request;
use serde_json::{json, Value};
use tower::ServiceExt as _;

use crate::contract::api::{
    BinaryKind, BinaryProbeResult, BinaryStatus, BinaryVariant, DownloadEntry,
    DownloadEntryDestination, DownloadPlan, DownloadStatus,
};
use crate::error::{AppError, ErrorCode, Result};
use crate::models::deps::{Downloads, ModelsDeps};
use crate::models::required::EMBED_MODEL_FILE;
use crate::models::Models;
use crate::settings::Settings;
use crate::storage::deps::RequirementsNotifier;
use crate::transport::state::AppContext;
use crate::transport::testing::{context_with, StubAuth};
use crate::transport::{Connection, FrameBus, Outbound};

use super::compute::{binary_source, binary_source_to_kind, required_binary_kinds};
use super::deps::Binaries;
use super::edges::{is_recompute_edge, DownloadEdges};
use super::Requirements;

const TOKEN: &str = "token-d8";
const CLIENT: &str = "client-d8";
const LLM_SPEC: &str = "@unsloth/Qwen3.5-2B-GGUF/main/Qwen3.5-2B-Q4_K_M.gguf";
const MMPROJ_SPEC: &str = "@unsloth/Qwen3.5-2B-GGUF/main/mmproj-F16.gguf";

// --- doubles -----------------------------------------------------------------

/// The download queue, as the snapshot uses it.
///
/// It records the `network` flag of every probe, because the consent rule is
/// the one thing here that cannot be checked by reading the answer: a snapshot
/// that reached HuggingFace and one that did not return the same shape.
#[derive(Default)]
struct FakeDownloads {
    /// Sources reported as already on disk.
    present: Vec<String>,
    /// Size hint per source, for the ones that are missing.
    sizes: HashMap<String, i64>,
    /// Rows the queue currently holds, already filtered to `models`.
    rows: Vec<DownloadEntry>,
    /// The `network` argument of every probe, in order.
    probes: Mutex<Vec<bool>>,
    enqueued: Mutex<Vec<(String, String)>>,
    /// Groups whose enqueue must fail, to prove one failure does not starve the
    /// others.
    enqueue_fails: Vec<String>,
    /// Held for the length of one probe, so a test can keep a recompute in
    /// flight while it fires more notifications at it.
    gate: Option<Arc<tokio::sync::Semaphore>>,
}

impl FakeDownloads {
    fn probe_networks(&self) -> Vec<bool> {
        self.probes.lock().unwrap().clone()
    }
}

#[async_trait]
impl Downloads for FakeDownloads {
    async fn probe(&self, sources: &[String], network: bool) -> Result<Vec<DownloadPlan>> {
        self.probes.lock().unwrap().push(network);
        if let Some(gate) = &self.gate {
            // Dropped immediately: this is a rendezvous, not a lock. The test
            // adds permits when it wants the recompute to proceed.
            let _permit = gate.acquire().await;
        }
        Ok(sources
            .iter()
            .map(|source| DownloadPlan {
                already_have: self.present.contains(source),
                error: None,
                size_hint: self.sizes.get(source).copied(),
                source: source.clone(),
                version: None,
            })
            .collect())
    }

    fn enqueue_models(&self, items: &[(String, String)]) -> Result<Vec<String>> {
        for (_, group) in items {
            if self.enqueue_fails.contains(group) {
                return Err(AppError::new(
                    ErrorCode::ValidationError,
                    format!("{group} refuses to enqueue"),
                ));
            }
        }
        self.enqueued.lock().unwrap().extend_from_slice(items);
        Ok(items
            .iter()
            .map(|(source, _)| format!("job:{source}"))
            .collect())
    }

    fn models_snapshot(&self) -> Vec<DownloadEntry> {
        self.rows.clone()
    }

    fn cancel(&self, _id: &str) {}
    fn retry(&self, _id: &str) {}
    fn remove(&self, _id: &str) {}

    async fn reconcile_completed(&self) -> Result<()> {
        Ok(())
    }
}

/// The binary installer, as the snapshot uses it.
#[derive(Default)]
struct FakeBinaries {
    status: Vec<BinaryStatus>,
    failures: HashMap<String, String>,
    probe_answer: Vec<BinaryProbeResult>,
    /// How many times `probe` reached upstream. The consent counter for the
    /// binary half.
    probe_calls: AtomicUsize,
    probed_kinds: Mutex<Vec<Vec<BinaryKind>>>,
    installed: Mutex<Vec<Vec<BinaryKind>>>,
    /// `list` fails, which is the unreadable-manifest case the oracle swallows.
    list_fails: bool,
}

#[async_trait]
impl Binaries for FakeBinaries {
    async fn list(&self) -> Result<Vec<BinaryStatus>> {
        if self.list_fails {
            return Err(AppError::new(
                ErrorCode::ManifestFetchFailed,
                "no manifest here",
            ));
        }
        Ok(self.status.clone())
    }

    async fn probe(&self, kinds: &[BinaryKind]) -> Result<Vec<BinaryProbeResult>> {
        self.probe_calls.fetch_add(1, Ordering::SeqCst);
        self.probed_kinds.lock().unwrap().push(kinds.to_vec());
        Ok(self.probe_answer.clone())
    }

    fn failure(&self, kind: BinaryKind) -> Option<String> {
        self.failures.get(&kind.to_string()).cloned()
    }

    async fn install(&self, kinds: Vec<BinaryKind>) -> Result<Vec<String>> {
        self.installed.lock().unwrap().push(kinds.clone());
        Ok(kinds.iter().map(|k| format!("bin:{k}")).collect())
    }
}

fn deno_status(installed: bool) -> BinaryStatus {
    BinaryStatus {
        kind: BinaryKind::Deno,
        version: "latest".to_string(),
        installed,
        path: installed.then(|| "/bin/deno".to_string()),
        variant: installed.then_some(BinaryVariant::Cpu),
        target: installed.then_some(BinaryVariant::Cpu),
        desired_variant: Some(BinaryVariant::Cpu),
    }
}

fn model_row(id: &str, source: &str, status: DownloadStatus, error: Option<&str>) -> DownloadEntry {
    DownloadEntry {
        abs_path: format!("/models/{id}"),
        added_at_ms: 0,
        destination: DownloadEntryDestination::Models,
        downloaded_bytes: 0,
        error: error.map(str::to_string),
        filename: id.to_string(),
        group_id: "llm".to_string(),
        id: id.to_string(),
        rel_path: id.to_string(),
        size_bytes: None,
        source: source.to_string(),
        status,
    }
}

// --- fixture -----------------------------------------------------------------

struct Env {
    ctx: AppContext,
    settings: Settings,
    downloads: Arc<FakeDownloads>,
    binaries: Arc<FakeBinaries>,
    requirements: Requirements,
    frames: Arc<FrameBus>,
}

fn env_with(downloads: FakeDownloads, binaries: FakeBinaries) -> Env {
    let ctx = context_with(Arc::new(
        StubAuth::new()
            .with_client(TOKEN, CLIENT)
            .with_admin_token("admin"),
    ));
    // `context_with` roots a `Paths` and stops there, so nothing on disk exists
    // yet. Settings writes and session documents both need the tree.
    ctx.paths.ensure_dirs().unwrap();
    let downloads = Arc::new(downloads);
    let binaries = Arc::new(binaries);
    let models = Models::new(
        &ctx,
        ModelsDeps::unwired().with_downloads(Arc::clone(&downloads) as Arc<dyn Downloads>),
    );
    let requirements = Requirements::new(
        ctx.settings.clone(),
        models,
        Arc::clone(&binaries) as Arc<dyn Binaries>,
        Arc::clone(&ctx.frames),
    );
    Env {
        settings: ctx.settings.clone(),
        frames: Arc::clone(&ctx.frames),
        ctx,
        downloads,
        binaries,
        requirements,
    }
}

/// Turn Image Attachments on, which is what makes the projector a required
/// file. It is off by the schema default, so a scenario that wants a projector
/// row has to ask for one: selecting a vision model writes `llm.mmprojPath`,
/// but loading (and therefore downloading) it is the user's separate answer.
async fn with_images(env: &Env) {
    env.settings
        .patch(map(json!({ "llm.supportImages": true })))
        .await
        .unwrap();
}

/// The common shape: nothing on disk, `deno` not installed.
fn env() -> Env {
    env_with(
        FakeDownloads::default(),
        FakeBinaries {
            status: vec![deno_status(false)],
            ..Default::default()
        },
    )
}

/// Drain exactly `n` frames, or FAIL.
///
/// Bounded, and not as a precaution: a connection with nothing queued parks
/// forever, so a frame that never arrives is a hang rather than a red test, and
/// a hang here wedges every other test in the binary. Copied in shape from
/// `extensions/install/runner.rs`'s `kinds`, which records the same hazard being
/// watched happen.
async fn frames_within(conn: &mut Connection, n: usize) -> Vec<Value> {
    let mut out = Vec::new();
    for i in 0..n {
        let item = tokio::time::timeout(Duration::from_secs(5), conn.next())
            .await
            .unwrap_or_else(|_| panic!("frame {} of {n} never arrived", i + 1));
        let Some(Outbound::Frame(json)) = item else {
            panic!("expected a frame, got {item:?}");
        };
        out.push(serde_json::from_str(&json).unwrap());
    }
    out
}

/// Assert that NO frame arrives inside a short window.
///
/// The negative half of `frames_within`. The window is short on purpose: this
/// answers "did the coalescing drop it", and every send on this path is
/// immediate once the recompute finishes.
async fn no_frame_within(conn: &mut Connection, window: Duration) {
    if let Ok(Some(Outbound::Frame(json))) = tokio::time::timeout(window, conn.next()).await {
        panic!("expected no frame, got {json}");
    }
}

fn sources(snap: &crate::contract::api::RequirementsSnapshot) -> Vec<&str> {
    snap.required.iter().map(|r| r.source.as_str()).collect()
}

fn missing_sources(snap: &crate::contract::api::RequirementsSnapshot) -> Vec<&str> {
    snap.missing.iter().map(|r| r.source.as_str()).collect()
}

fn row<'a>(
    snap: &'a crate::contract::api::RequirementsSnapshot,
    source: &str,
) -> &'a crate::contract::api::RequiredFile {
    snap.required
        .iter()
        .find(|r| r.source == source)
        .unwrap_or_else(|| panic!("no row for {source} in {:?}", sources(snap)))
}

// --- compute.rs: what the default configuration needs ------------------------

/// The whole shape in one assertion, because the ORDER is observable: the client
/// renders `required` as a list and `missing` as the popup's rows.
///
/// Mutation: swapping the two `extend` operands in `compute` puts the binary row
/// first and turns this red.
#[tokio::test]
async fn the_default_configuration_needs_two_model_files_and_the_worker_runtime() {
    let env = env();
    let snap = env.requirements.snapshot(false).await.unwrap();

    // The projector is deliberately NOT here. `llm.mmprojPath` is populated by
    // default, so the only thing keeping a 205 MB to 1.2 GB file out of a fresh
    // install's download gate is Image Attachments being off until asked for.
    assert_eq!(
        sources(&snap),
        vec![LLM_SPEC, EMBED_MODEL_FILE, "binary:deno"],
    );
    // Nothing is on disk in this fixture, so `missing` is the whole list. The
    // interesting half is that it IS the whole list: a filter inverted anywhere
    // would leave the app ungated.
    assert_eq!(missing_sources(&snap), sources(&snap));
    assert_eq!(row(&snap, LLM_SPEC).group.to_string(), "llm");
    assert_eq!(row(&snap, EMBED_MODEL_FILE).group.to_string(), "embed");
    assert_eq!(row(&snap, "binary:deno").group.to_string(), "binary");
    assert_eq!(row(&snap, LLM_SPEC).type_.to_string(), "model");
    assert_eq!(row(&snap, "binary:deno").type_.to_string(), "binary");
}

/// ...and turning Image Attachments on adds it. The pair is the point: the first
/// test alone passes against a build that dropped projector support entirely,
/// which is the difference between a default and a decision made for the user.
#[tokio::test]
async fn turning_image_attachments_on_adds_the_projector() {
    let env = env();
    with_images(&env).await;
    let snap = env.requirements.snapshot(false).await.unwrap();

    assert_eq!(
        sources(&snap),
        vec![LLM_SPEC, MMPROJ_SPEC, EMBED_MODEL_FILE, "binary:deno"],
    );
    assert_eq!(row(&snap, MMPROJ_SPEC).group.to_string(), "llm");
}

/// Present rows stay in `required` and drop out of `missing`. Both halves,
/// because a `missing` that was always empty would pass the second alone.
///
/// Mutation: `!r.present` -> `r.present` in `compute`.
#[tokio::test]
async fn a_file_already_on_disk_is_required_but_not_missing() {
    let env = env_with(
        FakeDownloads {
            present: vec![LLM_SPEC.to_string(), MMPROJ_SPEC.to_string()],
            ..Default::default()
        },
        FakeBinaries {
            status: vec![deno_status(true)],
            ..Default::default()
        },
    );
    with_images(&env).await;
    let snap = env.requirements.snapshot(false).await.unwrap();

    assert_eq!(sources(&snap).len(), 4, "every row stays in `required`");
    assert!(row(&snap, LLM_SPEC).present);
    assert!(row(&snap, "binary:deno").present);
    assert_eq!(missing_sources(&snap), vec![EMBED_MODEL_FILE]);
}

/// One source required by two groups yields ONE row, keeping the first group.
///
/// Mutation: drop the `seen.insert` filter and the same file is offered for
/// download twice.
#[tokio::test]
async fn a_source_required_twice_appears_once_under_the_first_group_that_wanted_it() {
    let env = env();
    // Point the projector at the embedding model, so `embed` and `llm` both name
    // it. Contrived as a configuration, and the only way to reach the branch
    // without a second model family in the fixture. Image Attachments has to be
    // on for the projector to be required at all.
    env.settings
        .patch(map(json!({
            "llm.mmprojPath": EMBED_MODEL_FILE,
            "llm.supportImages": true,
        })))
        .await
        .unwrap();

    let snap = env.requirements.snapshot(false).await.unwrap();
    assert_eq!(
        sources(&snap),
        vec![LLM_SPEC, EMBED_MODEL_FILE, "binary:deno"]
    );
    assert_eq!(
        row(&snap, EMBED_MODEL_FILE).group.to_string(),
        "llm",
        "the first group that wanted it wins, not the last",
    );
}

/// A failed model download surfaces its reason and stays missing, so the popup
/// can offer a retry instead of showing a file that is perpetually
/// "downloading".
///
/// Mutation: drop the `DownloadStatus::Error` guard and every row's error
/// becomes whatever the last queue row said.
#[tokio::test]
async fn a_failed_model_download_surfaces_its_error_and_is_still_missing() {
    let env = env_with(
        FakeDownloads {
            rows: vec![
                model_row("r1", LLM_SPEC, DownloadStatus::Error, Some("disk full")),
                // A row in flight for another file, to prove only the Error row
                // is read.
                model_row("r2", MMPROJ_SPEC, DownloadStatus::Downloading, None),
            ],
            ..Default::default()
        },
        FakeBinaries {
            status: vec![deno_status(false)],
            ..Default::default()
        },
    );
    with_images(&env).await;
    let snap = env.requirements.snapshot(false).await.unwrap();

    assert_eq!(row(&snap, LLM_SPEC).error.as_deref(), Some("disk full"));
    assert_eq!(row(&snap, MMPROJ_SPEC).error, None);
    assert!(missing_sources(&snap).contains(&LLM_SPEC));
}

/// An `Error` row with no message still says something, rather than rendering
/// as a normal missing file with no explanation.
#[tokio::test]
async fn a_failed_download_with_no_message_still_reads_as_failed() {
    let env = env_with(
        FakeDownloads {
            rows: vec![model_row("r1", LLM_SPEC, DownloadStatus::Error, None)],
            ..Default::default()
        },
        FakeBinaries::default(),
    );
    let snap = env.requirements.snapshot(false).await.unwrap();
    assert_eq!(
        row(&snap, LLM_SPEC).error.as_deref(),
        Some("download failed"),
    );
}

/// A file that IS present carries neither the stale error of an earlier failed
/// attempt nor a size hint: it is not offered for download, so both would be
/// noise the modal has to filter out again.
///
/// Mutation: drop the `(!is_present)` guard on either field.
#[tokio::test]
async fn a_present_file_carries_neither_an_error_nor_a_size() {
    let env = env_with(
        FakeDownloads {
            present: vec![LLM_SPEC.to_string()],
            sizes: HashMap::from([
                (LLM_SPEC.to_string(), 111),
                (EMBED_MODEL_FILE.to_string(), 222),
            ]),
            rows: vec![model_row(
                "r1",
                LLM_SPEC,
                DownloadStatus::Error,
                Some("old"),
            )],
            ..Default::default()
        },
        FakeBinaries::default(),
    );
    let snap = env.requirements.snapshot(true).await.unwrap();

    let present = row(&snap, LLM_SPEC);
    assert!(present.present);
    assert_eq!(present.error, None);
    assert_eq!(present.size_hint, None);
    // The paired half: the SAME probe answers a size for a missing file, so the
    // assertion above is about presence rather than about sizes never arriving.
    assert_eq!(row(&snap, EMBED_MODEL_FILE).size_hint, Some(222));
}

// --- compute.rs: the binary row ----------------------------------------------

/// Presence is judged by `target` (what the install AIMED for), not `variant`
/// (what landed). Both directions, because asserting only the degraded case
/// passes with the whole comparison deleted.
///
/// Mutation: compare `variant` instead of `target`. The degraded install then
/// reads as missing and the popup re-offers a download that can never settle.
#[tokio::test]
async fn a_degraded_install_is_present_and_a_stale_one_is_missing() {
    let degraded = BinaryStatus {
        // The install aimed for cuda, upstream had no cuda asset, cpu landed.
        variant: Some(BinaryVariant::Cpu),
        target: Some(BinaryVariant::Cuda),
        desired_variant: Some(BinaryVariant::Cuda),
        ..deno_status(true)
    };
    let env = env_with(
        FakeDownloads::default(),
        FakeBinaries {
            status: vec![degraded],
            ..Default::default()
        },
    );
    assert!(
        row(
            &env.requirements.snapshot(false).await.unwrap(),
            "binary:deno"
        )
        .present,
        "an unavoidable fallback settles rather than looping the popup",
    );

    let stale = BinaryStatus {
        // A GPU appeared since the last install: what it aimed for is no longer
        // what this device should have.
        variant: Some(BinaryVariant::Cpu),
        target: Some(BinaryVariant::Cpu),
        desired_variant: Some(BinaryVariant::Cuda),
        ..deno_status(true)
    };
    let env = env_with(
        FakeDownloads::default(),
        FakeBinaries {
            status: vec![stale],
            ..Default::default()
        },
    );
    assert!(
        !row(
            &env.requirements.snapshot(false).await.unwrap(),
            "binary:deno"
        )
        .present,
        "a genuinely new ideal drops into missing for the reinstall",
    );
}

/// When either side of the comparison is unknown, a plain install counts as
/// present, so the gate never blocks on an upgrade it cannot even name.
///
/// Mutation: make the `_` arm `false` and an offline core with no manifest entry
/// blocks the app forever.
#[tokio::test]
async fn an_install_with_no_recorded_variant_counts_as_present() {
    for (variant, target, desired) in [
        (None, None, None),
        (Some(BinaryVariant::Cpu), None, Some(BinaryVariant::Cuda)),
        (Some(BinaryVariant::Cpu), Some(BinaryVariant::Cpu), None),
    ] {
        let env = env_with(
            FakeDownloads::default(),
            FakeBinaries {
                status: vec![BinaryStatus {
                    variant,
                    target,
                    desired_variant: desired,
                    ..deno_status(true)
                }],
                ..Default::default()
            },
        );
        assert!(
            row(
                &env.requirements.snapshot(false).await.unwrap(),
                "binary:deno"
            )
            .present,
            "{variant:?}/{target:?}/{desired:?}",
        );
    }
}

/// A recorded install failure makes the row retryable with a reason attached,
/// and stops doing so the moment the binary is present.
///
/// Mutation: drop the `!is_present` guard on the binary `error`, and a working
/// core shows the reason its last failed attempt gave.
#[tokio::test]
async fn a_failed_binary_install_reads_as_retryable_until_it_succeeds() {
    let failures = HashMap::from([("deno".to_string(), "could not resolve upstream".to_string())]);
    let failed = env_with(
        FakeDownloads::default(),
        FakeBinaries {
            status: vec![deno_status(false)],
            failures: failures.clone(),
            ..Default::default()
        },
    );
    let snap = failed.requirements.snapshot(false).await.unwrap();
    assert_eq!(
        row(&snap, "binary:deno").error.as_deref(),
        Some("could not resolve upstream"),
    );
    assert!(missing_sources(&snap).contains(&"binary:deno"));

    let installed = env_with(
        FakeDownloads::default(),
        FakeBinaries {
            status: vec![deno_status(true)],
            failures,
            ..Default::default()
        },
    );
    let snap = installed.requirements.snapshot(false).await.unwrap();
    assert_eq!(row(&snap, "binary:deno").error, None);
}

/// A manifest this core cannot read must not take the whole snapshot down: every
/// model row in it is still correct, and the binary simply reads as missing.
///
/// Mutation: `?` instead of `.unwrap_or_default()` on `list`, and an unreadable
/// manifest turns `GET /api/v1/requirements` into a 500.
#[tokio::test]
async fn an_unreadable_binary_manifest_leaves_the_model_rows_intact() {
    let env = env_with(
        FakeDownloads {
            present: vec![LLM_SPEC.to_string()],
            ..Default::default()
        },
        FakeBinaries {
            list_fails: true,
            ..Default::default()
        },
    );
    let snap = env.requirements.snapshot(false).await.unwrap();
    assert!(row(&snap, LLM_SPEC).present);
    assert!(!row(&snap, "binary:deno").present);
}

/// `unavailable` is absent rather than `false`, matching the oracle's
/// `unavailable || undefined`. A key that is present-and-false is a wire change
/// even though both are falsy in the client's language.
#[tokio::test]
async fn an_available_binary_omits_the_unavailable_key_rather_than_sending_false() {
    let env = env();
    let snap = env.requirements.snapshot(false).await.unwrap();
    assert_eq!(row(&snap, "binary:deno").unavailable, None);
    let json = serde_json::to_value(&snap).unwrap();
    let binary = json["required"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["source"] == "binary:deno")
        .unwrap();
    assert!(binary.get("unavailable").is_none(), "{binary}");
}

/// The narrowed binary surface (Q2 / D7's A2), asserted rather than assumed.
///
/// This core absorbs `llama-server` and `loca-core-speech` in-process, so
/// offering them would put two permanently unsatisfiable rows in the app's
/// pending gate. Enabling local speech, which is what makes the oracle add
/// `loca-core-speech`, must not add a binary row here.
#[tokio::test]
async fn local_speech_adds_model_files_but_never_a_second_binary() {
    let env = env();
    env.settings
        .patch(map(json!({
            "stt.enabled": true,
            "stt.provider": "local",
            // A bundle: the setting stores a JSON document as a string, one
            // entry per file role.
            "stt.modelFiles": r#"{"encoder":"@csukuangfj/sherpa/main/encoder.onnx"}"#,
        })))
        .await
        .unwrap();

    let snap = env.requirements.snapshot(false).await.unwrap();
    assert!(
        sources(&snap).contains(&"@csukuangfj/sherpa/main/encoder.onnx"),
        "the speech MODEL is still required: {:?}",
        sources(&snap),
    );
    let binaries: Vec<&str> = snap
        .required
        .iter()
        .filter(|r| r.type_.to_string() == "binary")
        .map(|r| r.source.as_str())
        .collect();
    assert_eq!(binaries, vec!["binary:deno"]);
    assert_eq!(required_binary_kinds(), &[BinaryKind::Deno]);
}

#[test]
fn a_binary_source_round_trips_and_anything_else_is_not_one() {
    assert_eq!(binary_source(BinaryKind::Deno), "binary:deno");
    assert_eq!(binary_source_to_kind("binary:deno"), Some(BinaryKind::Deno),);
    // A model spec must never be read as a binary: the download route sends
    // whatever this returns to the installer.
    assert_eq!(binary_source_to_kind(LLM_SPEC), None);
    assert_eq!(binary_source_to_kind("binary:"), None);
    assert_eq!(binary_source_to_kind("binary:nonsense"), None);
}

// --- compute.rs: consent -----------------------------------------------------

/// The always-on snapshot reaches the network NOWHERE. This is the repo's one
/// absolute rule, and the failure mode is invisible in the response.
///
/// Mutation: pass `true` for `network` in `compute::models`, or drop the
/// `probe_sizes &&` guard on the binary probe. Either turns this red.
#[tokio::test]
async fn the_always_on_snapshot_makes_no_outbound_call() {
    let env = env();
    env.requirements.snapshot(false).await.unwrap();

    assert_eq!(
        env.downloads.probe_networks(),
        vec![false],
        "the model probe ran, and ran local-only",
    );
    assert_eq!(
        env.binaries.probe_calls.load(Ordering::SeqCst),
        0,
        "the binary probe resolves an upstream release and must not run here",
    );
}

/// The paired half of the assertion above: the SAME counters move when the user
/// opens the confirm modal. Without this, a core that never probed at all would
/// pass the consent test.
///
/// Mutation: hardcode `false` for `probe_sizes` anywhere on the path.
#[tokio::test]
async fn the_consented_probe_reaches_both_upstreams() {
    let env = env();
    env.requirements.snapshot(true).await.unwrap();

    assert_eq!(env.downloads.probe_networks(), vec![true]);
    assert_eq!(env.binaries.probe_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        env.binaries.probed_kinds.lock().unwrap().clone(),
        vec![vec![BinaryKind::Deno]],
    );
}

/// Nothing missing means nothing to size, so even the consented path makes no
/// binary call. The oracle's `missingAvailable.length > 0` guard.
#[tokio::test]
async fn a_probe_with_no_missing_binary_still_makes_no_binary_call() {
    let env = env_with(
        FakeDownloads::default(),
        FakeBinaries {
            status: vec![deno_status(true)],
            ..Default::default()
        },
    );
    env.requirements.snapshot(true).await.unwrap();
    assert_eq!(env.binaries.probe_calls.load(Ordering::SeqCst), 0);
}

/// A probe that could not resolve a version reports no version at all, rather
/// than the literal word "unknown" rendered next to a download button.
#[tokio::test]
async fn an_unresolvable_probe_reports_a_size_but_no_version() {
    let env = env_with(
        FakeDownloads::default(),
        FakeBinaries {
            status: vec![deno_status(false)],
            probe_answer: vec![BinaryProbeResult {
                kind: BinaryKind::Deno,
                size_bytes: Some(4096),
                version: "unknown".to_string(),
            }],
            ..Default::default()
        },
    );
    let snap = env.requirements.snapshot(true).await.unwrap();
    let deno = row(&snap, "binary:deno");
    assert_eq!(deno.size_hint, Some(4096));
    assert_eq!(deno.version, None);

    // Paired: a real version survives, so the assertion above is about the
    // sentinel rather than about versions never being carried.
    let env = env_with(
        FakeDownloads::default(),
        FakeBinaries {
            status: vec![deno_status(false)],
            probe_answer: vec![BinaryProbeResult {
                kind: BinaryKind::Deno,
                size_bytes: Some(4096),
                version: "v2.8.2".to_string(),
            }],
            ..Default::default()
        },
    );
    let snap = env.requirements.snapshot(true).await.unwrap();
    assert_eq!(row(&snap, "binary:deno").version.as_deref(), Some("v2.8.2"));
}

// --- edges.rs: the download-vs-present race guard ----------------------------

/// The table from `hub.test.ts:21-31`, ported.
///
/// Mutation: any single row of this flips when the guard or the terminal test is
/// changed, which is what makes it worth being a table rather than prose.
#[test]
fn only_a_model_row_entering_a_terminal_state_is_a_recompute_edge() {
    use DownloadEntryDestination::{Binaries, Models};
    use DownloadStatus::{Completed, Downloading, Error, Pending};

    // BINARY rows never recompute, whatever they do. Their `Completed` is
    // PRE-EXTRACT, so recomputing there races a stale "still missing" answer
    // against the real post-extract one.
    assert!(!is_recompute_edge(Binaries, Some(Downloading), Completed));
    assert!(!is_recompute_edge(Binaries, Some(Downloading), Error));
    assert!(!is_recompute_edge(Binaries, None, Completed));

    // MODEL rows recompute on ENTERING a terminal state.
    assert!(is_recompute_edge(Models, Some(Downloading), Completed));
    assert!(is_recompute_edge(Models, Some(Downloading), Error));
    assert!(is_recompute_edge(Models, None, Completed));
    assert!(is_recompute_edge(Models, None, Error));

    // and not on anything else.
    assert!(!is_recompute_edge(Models, Some(Pending), Downloading));
    assert!(!is_recompute_edge(Models, Some(Completed), Completed));
    assert!(!is_recompute_edge(Models, Some(Error), Error));
    assert!(!is_recompute_edge(Models, Some(Completed), Error));
}

/// The whole-snapshot behaviours the per-row table cannot see.
///
/// Mutation: drop the `last.retain` / length comparison and the "user cleared a
/// completed download" case stops recomputing, so a model file deleted with its
/// row never reappears in the popup.
#[test]
fn a_vanished_row_recomputes_and_a_progress_tick_does_not() {
    let edges = DownloadEdges::new();
    let downloading = model_row("r1", LLM_SPEC, DownloadStatus::Downloading, None);
    let completed = model_row("r1", LLM_SPEC, DownloadStatus::Completed, None);

    // First sight of a non-terminal row is not an edge.
    assert!(!edges.note(std::slice::from_ref(&downloading)));
    // A progress tick on the same row is not an edge either.
    assert!(!edges.note(std::slice::from_ref(&downloading)));
    // Reaching Completed is.
    assert!(edges.note(std::slice::from_ref(&completed)));
    // Staying Completed is not.
    assert!(!edges.note(std::slice::from_ref(&completed)));
    // The row disappearing is, because clearing a completed download can
    // re-expose the file it downloaded as missing.
    assert!(edges.note(&[]));
    // And once it is gone, an empty snapshot is not an edge forever after.
    assert!(!edges.note(&[]));
}

/// A binaries row moving next to a model row must not mask the model row's
/// edge, and must not manufacture one of its own.
#[test]
fn a_binary_row_neither_masks_nor_manufactures_a_model_edge() {
    let edges = DownloadEdges::new();
    let mut binary = model_row("b1", "binary:deno", DownloadStatus::Downloading, None);
    binary.destination = DownloadEntryDestination::Binaries;
    let model = model_row("m1", LLM_SPEC, DownloadStatus::Downloading, None);
    assert!(!edges.note(&[binary.clone(), model.clone()]));

    let mut binary_done = binary.clone();
    binary_done.status = DownloadStatus::Completed;
    assert!(
        !edges.note(&[binary_done.clone(), model.clone()]),
        "a binary reaching Completed is pre-extract and must not recompute",
    );

    let mut model_done = model.clone();
    model_done.status = DownloadStatus::Completed;
    assert!(
        edges.note(&[binary_done, model_done]),
        "and the model row alongside it still recomputes",
    );
}

// --- the notifier seam -------------------------------------------------------

/// `changed()` through the ERASED trait object, which is the only form the two
/// dep structs hold it in.
///
/// This is the half D8 owns of the finding that four existing
/// `requirements.changed()` call sites notify nothing: `models/routes.rs:330,363`
/// and `storage/mod.rs:118,147` all call through `Arc<dyn RequirementsNotifier>`,
/// and boot currently leaves both dep structs at `Unwired`. Filling them is an
/// orchestrator edit, so what goes red HERE is the trait-object path itself; the
/// test that goes red if BOOT leaves the seam unwired is the conformance case
/// `d8.requirements.a-settings-change-broadcasts-a-snapshot`, which drives a real
/// core over the wire and cannot be satisfied by anything a unit test builds.
///
/// Mutation: make `changed()` a no-op body. This is the only test in the suite
/// that notices.
#[tokio::test]
async fn a_notification_through_the_erased_seam_broadcasts_a_snapshot() {
    let env = env();
    let mut conn = env.frames.register("watcher");
    let notifier: Arc<dyn RequirementsNotifier> = Arc::new(env.requirements.clone());

    notifier.changed();

    let frames = frames_within(&mut conn, 1).await;
    assert_eq!(frames[0]["kind"], "requirements.snapshot");
    // The payload is the real snapshot, not an empty envelope: a broadcast that
    // fired with nothing in it would still satisfy a kind-only assertion.
    let required = frames[0]["required"].as_array().unwrap();
    assert_eq!(required.len(), 3);
    assert_eq!(frames[0]["missing"].as_array().unwrap().len(), 3);
    assert_eq!(required[0]["source"], LLM_SPEC);
}

/// The recompute goes to EVERY client, because the requirements are core-global.
///
/// Mutation: `broadcast_to_client` instead of `broadcast_all`, which would leave
/// every other paired device showing a stale download popup.
#[tokio::test]
async fn the_snapshot_goes_to_every_connected_client() {
    let env = env();
    let mut one = env.frames.register("c1");
    let mut two = env.frames.register("c2");

    env.requirements.changed();

    assert_eq!(
        frames_within(&mut one, 1).await[0]["kind"],
        "requirements.snapshot"
    );
    assert_eq!(
        frames_within(&mut two, 1).await[0]["kind"],
        "requirements.snapshot"
    );
}

/// A burst of notifications costs at most TWO recomputes: the one running and
/// one trailing rerun. That is the bound `requirements.ts:150-172` states, and
/// without it applying a settings preset broadcasts once per key.
///
/// The gate is what makes this deterministic: the first recompute parks inside
/// `probe` until the test lets it go, so every other notification lands while
/// one is genuinely in flight.
///
/// Mutation: give the channel a larger buffer (`mpsc::channel(8)`), and the
/// probe count becomes 6 rather than 2.
#[tokio::test]
async fn a_burst_of_notifications_coalesces_into_at_most_two_recomputes() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let env = env_with(
        FakeDownloads {
            gate: Some(Arc::clone(&gate)),
            ..Default::default()
        },
        FakeBinaries::default(),
    );
    let mut conn = env.frames.register("watcher");

    // The first notification, then a bounded wait until the worker is genuinely
    // PARKED inside `probe`. Without this the six sends can all land before the
    // worker polls once, which leaves one queued rather than one running, and
    // the test would be asserting a race rather than the coalescing.
    env.requirements.changed();
    wait_until(|| env.downloads.probe_networks().len() == 1).await;

    for _ in 0..5 {
        env.requirements.changed();
    }
    // One permit is enough for any number of sequential probes: `acquire`
    // returns it on drop. Its only job is to unpark the recompute that is
    // already waiting.
    gate.add_permits(1);

    let frames = frames_within(&mut conn, 2).await;
    assert_eq!(frames.len(), 2);
    // And no third: the remaining four notifications were coalesced away rather
    // than queued behind the first two.
    no_frame_within(&mut conn, Duration::from_millis(200)).await;
    assert_eq!(
        env.downloads.probe_networks().len(),
        2,
        "six notifications, two recomputes",
    );
}

/// Poll a condition, BOUNDED. A condition that never comes true is a failure
/// naming itself, not a spin that burns a core for the life of the suite.
async fn wait_until(mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition never became true",
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A recompute that FAILS logs and broadcasts nothing, rather than telling every
/// client the requirements changed to something wrong.
///
/// Mutation: broadcast an empty snapshot on the error arm. The clients would
/// then see zero requirements and drop the pending gate.
#[tokio::test]
async fn a_failed_recompute_broadcasts_nothing() {
    // `ModelsDeps::unwired()` refuses to probe, which is exactly the failure a
    // real unwired download seam produces.
    let ctx = context_with(Arc::new(StubAuth::new()));
    let requirements = Requirements::new(
        ctx.settings.clone(),
        Models::new(&ctx, ModelsDeps::unwired()),
        Arc::new(FakeBinaries::default()) as Arc<dyn Binaries>,
        Arc::clone(&ctx.frames),
    );
    let mut conn = ctx.frames.register("watcher");

    requirements.changed();
    no_frame_within(&mut conn, Duration::from_millis(300)).await;

    // Paired: the same handle with a working seam DOES broadcast, so the
    // assertion above is about the failure rather than about the worker never
    // running.
    let env = env();
    let mut ok = env.frames.register("watcher");
    env.requirements.changed();
    assert_eq!(
        frames_within(&mut ok, 1).await[0]["kind"],
        "requirements.snapshot"
    );
}

// --- routes ------------------------------------------------------------------

fn app(env: &Env, greetings: super::Greetings) -> axum::Router {
    super::routes::routes(&env.ctx, env.requirements.clone(), greetings)
}

/// A `Greetings` with no chat behind it, for the two routes that never touch it.
fn no_greetings(env: &Env) -> super::Greetings {
    super::Greetings::new(chat_for(&env.ctx), Arc::new(NeverLoading))
}

async fn call(app: &axum::Router, req: Request<Body>) -> (axum::http::StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Mutation: drop the `require_client` layer from `routes()`.
#[tokio::test]
async fn every_route_in_the_slice_needs_a_bearer() {
    let env = env();
    let app = app(&env, no_greetings(&env));
    for (method, path) in [
        ("GET", "/api/v1/requirements"),
        ("POST", "/api/v1/requirements/download"),
        ("POST", "/api/v1/greetings/run"),
    ] {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let (status, body) = call(&app, req).await;
        assert_eq!(status, 401, "{method} {path}");
        assert_eq!(body["error"]["code"], "missing_token", "{method} {path}");
    }
}

/// `?probe=1` and NOTHING else turns on the outbound size probe.
///
/// The literal is the contract (`routes/requirements.ts:22`), and this is the
/// switch between a local-only snapshot and one that reaches HuggingFace and
/// GitHub, so a "helpful" widening to `?probe=true` would silently put an
/// unconsented request on the always-on path.
///
/// Mutation: `.is_some()` instead of `== Some("1")`.
#[tokio::test]
async fn only_the_literal_probe_1_turns_on_the_network() {
    let env = env();
    let app = app(&env, no_greetings(&env));

    for path in [
        "/api/v1/requirements",
        "/api/v1/requirements?probe=true",
        "/api/v1/requirements?probe=0",
        "/api/v1/requirements?probe=",
        "/api/v1/requirements?probe=11",
    ] {
        let (status, _) = call(&app, get(path)).await;
        assert_eq!(status, 200, "{path}");
    }
    assert!(
        env.downloads.probe_networks().iter().all(|n| !n),
        "{:?}",
        env.downloads.probe_networks(),
    );
    assert_eq!(env.binaries.probe_calls.load(Ordering::SeqCst), 0);

    let (status, body) = call(&app, get("/api/v1/requirements?probe=1")).await;
    assert_eq!(status, 200);
    assert_eq!(body["required"].as_array().unwrap().len(), 3);
    assert_eq!(env.downloads.probe_networks().last(), Some(&true));
    assert_eq!(env.binaries.probe_calls.load(Ordering::SeqCst), 1);
}

/// The download route enqueues one `ensure` per missing GROUP, not per file.
///
/// Mutation: collect per source instead of per group, and the two llm files
/// enqueue twice.
#[tokio::test]
async fn the_download_route_ensures_each_missing_group_once_and_installs_the_binaries() {
    let env = env();
    // Image Attachments on, so llm contributes TWO files and the per-group
    // batching is observable at all.
    with_images(&env).await;
    let app = app(&env, no_greetings(&env));

    let (status, body) = call(&app, post("/api/v1/requirements/download", json!({}))).await;
    assert_eq!(status, 200);

    // Two groups (llm, embed), and llm's two files enqueue together.
    let enqueued = env.downloads.enqueued.lock().unwrap().clone();
    assert_eq!(
        enqueued,
        vec![
            (LLM_SPEC.to_string(), "llm".to_string()),
            (MMPROJ_SPEC.to_string(), "llm".to_string()),
            (EMBED_MODEL_FILE.to_string(), "embed".to_string()),
        ],
    );
    assert_eq!(
        body["modelJobIds"],
        json!([
            format!("job:{LLM_SPEC}"),
            format!("job:{MMPROJ_SPEC}"),
            format!("job:{EMBED_MODEL_FILE}"),
        ]),
    );
    assert_eq!(
        env.binaries.installed.lock().unwrap().clone(),
        vec![vec![BinaryKind::Deno]],
    );
    assert_eq!(body["binaryJobIds"], json!(["bin:deno"]));
}

/// One group failing to enqueue must not starve the other groups OR the binary
/// installs behind it.
///
/// Mutation: `?` instead of the logged `match` arm, and a single malformed
/// source spec takes the whole confirm-modal button down.
#[tokio::test]
async fn one_failing_group_does_not_starve_the_others() {
    let env = env_with(
        FakeDownloads {
            enqueue_fails: vec!["llm".to_string()],
            ..Default::default()
        },
        FakeBinaries {
            status: vec![deno_status(false)],
            ..Default::default()
        },
    );
    let app = app(&env, no_greetings(&env));

    let (status, body) = call(&app, post("/api/v1/requirements/download", json!({}))).await;
    assert_eq!(status, 200);
    assert_eq!(
        body["modelJobIds"],
        json!([format!("job:{EMBED_MODEL_FILE}")])
    );
    assert_eq!(body["binaryJobIds"], json!(["bin:deno"]));
}

/// Nothing missing means no install call at all, and that is not tidiness:
/// `install` loads the binary manifest, which FETCHES with no cache, so calling
/// it with an empty list would put an outbound request behind a request that has
/// nothing to download.
///
/// Mutation: always call `install`, and this counts one.
#[tokio::test]
async fn a_download_with_no_missing_binary_never_calls_the_installer() {
    let env = env_with(
        FakeDownloads {
            present: vec![
                LLM_SPEC.to_string(),
                MMPROJ_SPEC.to_string(),
                EMBED_MODEL_FILE.to_string(),
            ],
            ..Default::default()
        },
        FakeBinaries {
            status: vec![deno_status(true)],
            ..Default::default()
        },
    );
    let app = app(&env, no_greetings(&env));

    let (status, body) = call(&app, post("/api/v1/requirements/download", json!({}))).await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({ "binaryJobIds": [], "modelJobIds": [] }));
    assert!(env.binaries.installed.lock().unwrap().is_empty());
    assert!(env.downloads.enqueued.lock().unwrap().is_empty());
}

// --- greetings ---------------------------------------------------------------

struct NeverLoading;

#[async_trait]
impl crate::chat::LoadingProbe for NeverLoading {
    async fn still_loading(&self) -> bool {
        false
    }
}

/// A probe the test drives: loading until it is told otherwise.
struct GatedLoading(Arc<std::sync::atomic::AtomicBool>);

#[async_trait]
impl crate::chat::LoadingProbe for GatedLoading {
    async fn still_loading(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A `Chat` over this context's own store, settings and frame bus, with a
/// scripted provider so an automated turn completes without a model.
fn chat_for(ctx: &AppContext) -> crate::chat::Chat {
    let llm = crate::llm::Llm::with_provider(
        ctx.vault.clone(),
        ctx.config.channel,
        Arc::new(crate::llm::testing::ScriptedProvider::text("hello there")),
    );
    crate::chat::Chat::new(crate::chat::ChatConfig {
        sessions: crate::sessions::SessionsStore::new(Arc::clone(&ctx.paths)),
        memories: crate::memories::MemoriesStore::new(Arc::clone(&ctx.paths), ctx.db.clone()),
        embedding: crate::memories::Embedding::unwired(ctx.settings.clone(), ctx.vault.clone()),
        settings: ctx.settings.clone(),
        llm,
        frames: Arc::clone(&ctx.frames),
        deps: crate::chat::deps::ChatDeps::unwired(),
        summaries: Arc::new(crate::memories::indexer::NoSummaries),
    })
}

fn fast_timing() -> super::greetings::Timing {
    super::greetings::Timing {
        ready_timeout: Duration::from_millis(400),
        poll: Duration::from_millis(5),
    }
}

/// The dedup window, in both directions. A crash-looping client must not mint a
/// session per retry, and a client past its window must still get one.
///
/// Mutation: drop the `last.insert`, and the second call runs too.
#[tokio::test]
async fn a_second_greeting_inside_the_window_is_suppressed_and_says_why() {
    let env = env();
    let greetings =
        super::Greetings::with_timing(chat_for(&env.ctx), Arc::new(NeverLoading), fast_timing());
    let app = app(&env, greetings);

    // `cooldownMs: 0` throughout: it is floored to 1s server-side, which is the
    // smallest window a client can actually get and therefore the only one a
    // test can wait out. With no `cooldownMs` the window is 60s.
    let short = || json!({ "cooldownMs": 0 });

    let (status, body) = call(&app, post("/api/v1/greetings/run", short())).await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({ "ran": true }),
        "and no `reason` key when it ran"
    );

    let (status, body) = call(&app, post("/api/v1/greetings/run", short())).await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({ "ran": false, "reason": "recent" }));

    // A window that has elapsed lets the next one through.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (_, body) = call(&app, post("/api/v1/greetings/run", short())).await;
    assert_eq!(body, json!({ "ran": true }));

    // And the DEFAULT window is the LONG one. Another floor's worth of time
    // passes, which is enough to reopen a 1s window and nowhere near enough to
    // reopen a 60s one, so a request that sends no `cooldownMs` is still
    // suppressed. Without this second sleep the assertion would also pass with
    // the default shortened to the floor, which is a mutation that survived.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (_, body) = call(&app, post("/api/v1/greetings/run", json!({}))).await;
    assert_eq!(body, json!({ "ran": false, "reason": "recent" }));
}

/// The floor, which is what stops a rogue client driving its own cooldown to
/// zero and spamming sessions.
///
/// Mutation: drop the `.max(INTERVAL_FLOOR_MS)` and the second call runs.
#[tokio::test]
async fn a_client_cannot_ask_for_a_zero_length_cooldown() {
    let env = env();
    let greetings =
        super::Greetings::with_timing(chat_for(&env.ctx), Arc::new(NeverLoading), fast_timing());
    let app = app(&env, greetings);

    call(
        &app,
        post("/api/v1/greetings/run", json!({ "cooldownMs": 0 })),
    )
    .await;
    let (_, body) = call(
        &app,
        post("/api/v1/greetings/run", json!({ "cooldownMs": 0 })),
    )
    .await;
    assert_eq!(
        body,
        json!({ "ran": false, "reason": "recent" }),
        "0 is floored to 1000ms, so an immediate retry is still suppressed",
    );
}

/// The dedup is PER CLIENT: one device greeting must not suppress another's.
///
/// Mutation: key the map on a constant instead of the client id.
#[tokio::test]
async fn one_client_greeting_does_not_suppress_another() {
    let env = env();
    let greetings =
        super::Greetings::with_timing(chat_for(&env.ctx), Arc::new(NeverLoading), fast_timing());

    assert!(greetings.run("client-a", Default::default()).ran);
    assert!(!greetings.run("client-a", Default::default()).ran);
    assert!(greetings.run("client-b", Default::default()).ran);
}

/// The body is strict, matching the route-local `.strict()` schema.
///
/// Mutation: drop `deny_unknown_fields`.
#[tokio::test]
async fn an_unknown_field_in_the_greeting_body_is_refused() {
    let env = env();
    let greetings =
        super::Greetings::with_timing(chat_for(&env.ctx), Arc::new(NeverLoading), fast_timing());
    let app = app(&env, greetings);

    let (status, body) = call(
        &app,
        post("/api/v1/greetings/run", json!({ "sessionTitel": "typo" })),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["code"], "validation_error");
}

/// The whole automated-session shape, observed from the wire: the frame the
/// client navigates on, and the transcript the session was born with.
///
/// Mutation: drop the `automated: true` flag, or send `focus: "show"`, or omit
/// the `session.created` broadcast entirely. Each turns a different line red.
#[tokio::test]
async fn a_greeting_opens_an_automated_session_and_announces_it() {
    let env = env();
    let chat = chat_for(&env.ctx);
    let greetings =
        super::Greetings::with_timing(chat.clone(), Arc::new(NeverLoading), fast_timing());
    let app = app(&env, greetings);
    let mut conn = env.frames.register(CLIENT);

    let (status, _) = call(
        &app,
        post(
            "/api/v1/greetings/run",
            json!({ "sessionTitle": "Hello {datetime}", "instruction": "  say hi  " }),
        ),
    )
    .await;
    assert_eq!(status, 200);

    let created = frames_within(&mut conn, 1).await.remove(0);
    assert_eq!(created["kind"], "session.created");
    assert_eq!(created["reason"], "greeting");
    assert_eq!(created["focus"], "show_when_done");
    assert!(
        created.get("scheduledPromptId").is_none(),
        "a greeting has no schedule behind it: {created}",
    );
    let title = created["session"]["title"].as_str().unwrap();
    assert!(title.starts_with("Hello 20"), "{title}");
    assert!(!title.contains("{datetime}"), "{title}");

    let session_id = created["session"]["id"].as_str().unwrap();
    let messages = chat_store(&env.ctx).list_messages(session_id).await;
    let first = messages.first().expect("the instruction was persisted");
    let map = first.as_map();
    assert_eq!(map["role"], json!("user"));
    assert_eq!(map["content"], json!("say hi"), "trimmed, not raw");
    assert_eq!(map["automated"], json!(true));
    assert_eq!(map["ord"], json!(0));
    assert!(map["id"].as_str().is_some_and(|id| !id.is_empty()));
}

/// The session store the `Chat` in [`chat_for`] writes through. Same paths, so
/// the same documents.
fn chat_store(ctx: &AppContext) -> crate::sessions::SessionsStore {
    crate::sessions::SessionsStore::new(Arc::clone(&ctx.paths))
}

/// The turn is deferred while the model loads, and starts once it is up.
///
/// Both halves in one test on purpose: "no session yet" alone passes against a
/// greeting that never runs at all.
///
/// Mutation: delete the `while probe.still_loading()` loop, and the session
/// appears in the first window.
#[tokio::test]
async fn a_greeting_waits_for_the_model_and_then_runs() {
    let env = env();
    let loading = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let greetings = super::Greetings::with_timing(
        chat_for(&env.ctx),
        Arc::new(GatedLoading(Arc::clone(&loading))),
        fast_timing(),
    );
    let mut conn = env.frames.register(CLIENT);

    assert!(greetings.run(CLIENT, Default::default()).ran);
    no_frame_within(&mut conn, Duration::from_millis(100)).await;

    loading.store(false, Ordering::SeqCst);
    let created = frames_within(&mut conn, 1).await.remove(0);
    assert_eq!(created["kind"], "session.created");
}

/// A model that never finishes loading must not swallow the greeting forever:
/// past the budget it starts anyway, so an error surfaces IN the session where
/// the user can see it.
///
/// Mutation: drop the deadline check from the loop, and this times out.
#[tokio::test]
async fn a_model_that_never_loads_stops_holding_the_greeting_past_the_budget() {
    let env = env();
    let greetings = super::Greetings::with_timing(
        chat_for(&env.ctx),
        Arc::new(GatedLoading(Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        )))),
        fast_timing(),
    );
    let mut conn = env.frames.register(CLIENT);

    assert!(greetings.run(CLIENT, Default::default()).ran);
    let created = frames_within(&mut conn, 1).await.remove(0);
    assert_eq!(created["kind"], "session.created");
}

fn map(value: Value) -> crate::settings::SettingsRecord {
    value.as_object().unwrap().clone()
}
