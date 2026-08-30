//! The requirements suite. Every wait is bounded ([`frames_within`]), every
//! "no network" assertion is paired with one that counts a real call, and route
//! tests mount the slice through [`routes::routes`].

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

/// The download queue, recording the `network` flag of every probe.
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
    /// Groups whose enqueue must fail.
    enqueue_fails: Vec<String>,
    /// Held for the length of one probe, to keep a recompute in flight.
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
            // A rendezvous, not a lock.
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
    /// How many times `probe` reached upstream.
    probe_calls: AtomicUsize,
    probed_kinds: Mutex<Vec<Vec<BinaryKind>>>,
    installed: Mutex<Vec<Vec<BinaryKind>>>,
    /// `list` fails (the unreadable-manifest case).
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

/// Turn Image Attachments on, which is what makes the projector required.
async fn with_images(env: &Env) {
    env.settings
        .patch(map(json!({ "llm.supportImages": true })))
        .await
        .unwrap();
}

/// The common shape: nothing on disk, `deno` not installed.
fn env() -> Env {
    env_bins(deno_status(false))
}

fn env_bins(status: BinaryStatus) -> Env {
    env_with(
        FakeDownloads::default(),
        FakeBinaries {
            status: vec![status],
            ..Default::default()
        },
    )
}

/// `env_with`, with the binaries reduced to `deno` installed or not.
fn env_deno(downloads: FakeDownloads, installed: bool) -> Env {
    env_with(
        downloads,
        FakeBinaries {
            status: vec![deno_status(installed)],
            ..Default::default()
        },
    )
}

async fn deno_present(env: &Env) -> bool {
    row(
        &env.requirements.snapshot(false).await.unwrap(),
        "binary:deno",
    )
    .present
}

/// Drain exactly `n` frames, or fail: an unbounded wait would hang the binary.
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

/// Assert that no frame arrives inside a short window.
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

#[tokio::test]
async fn the_default_configuration_needs_two_model_files_and_the_worker_runtime() {
    let env = env();
    let snap = env.requirements.snapshot(false).await.unwrap();

    assert_eq!(
        sources(&snap),
        vec![LLM_SPEC, EMBED_MODEL_FILE, "binary:deno"],
    );
    assert_eq!(missing_sources(&snap), sources(&snap));
    assert_eq!(row(&snap, LLM_SPEC).group.to_string(), "llm");
    assert_eq!(row(&snap, EMBED_MODEL_FILE).group.to_string(), "embed");
    assert_eq!(row(&snap, "binary:deno").group.to_string(), "binary");
    assert_eq!(row(&snap, LLM_SPEC).type_.to_string(), "model");
    assert_eq!(row(&snap, "binary:deno").type_.to_string(), "binary");
}

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

#[tokio::test]
async fn a_file_already_on_disk_is_required_but_not_missing() {
    let env = env_deno(
        FakeDownloads {
            present: vec![LLM_SPEC.to_string(), MMPROJ_SPEC.to_string()],
            ..Default::default()
        },
        true,
    );
    with_images(&env).await;
    let snap = env.requirements.snapshot(false).await.unwrap();

    assert_eq!(sources(&snap).len(), 4, "every row stays in `required`");
    assert!(row(&snap, LLM_SPEC).present);
    assert!(row(&snap, "binary:deno").present);
    assert_eq!(missing_sources(&snap), vec![EMBED_MODEL_FILE]);
}

#[tokio::test]
async fn a_source_required_twice_appears_once_under_the_first_group_that_wanted_it() {
    let env = env();
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

#[tokio::test]
async fn a_failed_model_download_surfaces_its_error_and_is_still_missing() {
    let env = env_deno(
        FakeDownloads {
            rows: vec![
                model_row("r1", LLM_SPEC, DownloadStatus::Error, Some("disk full")),
                model_row("r2", MMPROJ_SPEC, DownloadStatus::Downloading, None),
            ],
            ..Default::default()
        },
        false,
    );
    with_images(&env).await;
    let snap = env.requirements.snapshot(false).await.unwrap();

    assert_eq!(row(&snap, LLM_SPEC).error.as_deref(), Some("disk full"));
    assert_eq!(row(&snap, MMPROJ_SPEC).error, None);
    assert!(missing_sources(&snap).contains(&LLM_SPEC));
}

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
    assert_eq!(row(&snap, EMBED_MODEL_FILE).size_hint, Some(222));
}

// --- compute.rs: the binary row ----------------------------------------------

#[tokio::test]
async fn a_degraded_install_is_present_and_a_stale_one_is_missing() {
    let degraded = BinaryStatus {
        variant: Some(BinaryVariant::Cpu),
        target: Some(BinaryVariant::Cuda),
        desired_variant: Some(BinaryVariant::Cuda),
        ..deno_status(true)
    };
    assert!(
        deno_present(&env_bins(degraded)).await,
        "an unavoidable fallback settles rather than looping the popup",
    );

    let stale = BinaryStatus {
        variant: Some(BinaryVariant::Cpu),
        target: Some(BinaryVariant::Cpu),
        desired_variant: Some(BinaryVariant::Cuda),
        ..deno_status(true)
    };
    assert!(
        !deno_present(&env_bins(stale)).await,
        "a genuinely new ideal drops into missing for the reinstall",
    );
}

#[tokio::test]
async fn an_install_with_no_recorded_variant_counts_as_present() {
    for (variant, target, desired) in [
        (None, None, None),
        (Some(BinaryVariant::Cpu), None, Some(BinaryVariant::Cuda)),
        (Some(BinaryVariant::Cpu), Some(BinaryVariant::Cpu), None),
    ] {
        let env = env_bins(BinaryStatus {
            variant,
            target,
            desired_variant: desired,
            ..deno_status(true)
        });
        assert!(
            deno_present(&env).await,
            "{variant:?}/{target:?}/{desired:?}",
        );
    }
}

#[tokio::test]
async fn a_failed_binary_install_reads_as_retryable_until_it_succeeds() {
    let failures = HashMap::from([("deno".to_string(), "could not resolve upstream".to_string())]);
    let with_failure = |installed: bool| {
        env_with(
            FakeDownloads::default(),
            FakeBinaries {
                status: vec![deno_status(installed)],
                failures: failures.clone(),
                ..Default::default()
            },
        )
    };
    let snap = with_failure(false)
        .requirements
        .snapshot(false)
        .await
        .unwrap();
    assert_eq!(
        row(&snap, "binary:deno").error.as_deref(),
        Some("could not resolve upstream"),
    );
    assert!(missing_sources(&snap).contains(&"binary:deno"));

    let snap = with_failure(true)
        .requirements
        .snapshot(false)
        .await
        .unwrap();
    assert_eq!(row(&snap, "binary:deno").error, None);
}

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

#[tokio::test]
async fn local_speech_adds_model_files_but_never_a_second_binary() {
    let env = env();
    env.settings
        .patch(map(json!({
            "stt.enabled": true,
            "stt.provider": "local",
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
    assert_eq!(binary_source_to_kind(LLM_SPEC), None);
    assert_eq!(binary_source_to_kind("binary:"), None);
    assert_eq!(binary_source_to_kind("binary:nonsense"), None);
}

// --- compute.rs: consent -----------------------------------------------------

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

#[tokio::test]
async fn a_probe_with_no_missing_binary_still_makes_no_binary_call() {
    let env = env_bins(deno_status(true));
    env.requirements.snapshot(true).await.unwrap();
    assert_eq!(env.binaries.probe_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_unresolvable_probe_reports_a_size_but_no_version() {
    let probed = |version: &str| {
        env_with(
            FakeDownloads::default(),
            FakeBinaries {
                status: vec![deno_status(false)],
                probe_answer: vec![BinaryProbeResult {
                    kind: BinaryKind::Deno,
                    size_bytes: Some(4096),
                    version: version.to_string(),
                }],
                ..Default::default()
            },
        )
    };
    let snap = probed("unknown").requirements.snapshot(true).await.unwrap();
    let deno = row(&snap, "binary:deno");
    assert_eq!(deno.size_hint, Some(4096));
    assert_eq!(deno.version, None);

    let snap = probed("v2.8.2").requirements.snapshot(true).await.unwrap();
    assert_eq!(row(&snap, "binary:deno").version.as_deref(), Some("v2.8.2"));
}

// --- edges.rs: the download-vs-present race guard ----------------------------

#[test]
fn only_a_model_row_entering_a_terminal_state_is_a_recompute_edge() {
    use DownloadEntryDestination::{Binaries, Models};
    use DownloadStatus::{Completed, Downloading, Error, Pending};

    assert!(!is_recompute_edge(Binaries, Some(Downloading), Completed));
    assert!(!is_recompute_edge(Binaries, Some(Downloading), Error));
    assert!(!is_recompute_edge(Binaries, None, Completed));

    assert!(is_recompute_edge(Models, Some(Downloading), Completed));
    assert!(is_recompute_edge(Models, Some(Downloading), Error));
    assert!(is_recompute_edge(Models, None, Completed));
    assert!(is_recompute_edge(Models, None, Error));

    assert!(!is_recompute_edge(Models, Some(Pending), Downloading));
    assert!(!is_recompute_edge(Models, Some(Completed), Completed));
    assert!(!is_recompute_edge(Models, Some(Error), Error));
    assert!(!is_recompute_edge(Models, Some(Completed), Error));
}

#[test]
fn a_vanished_row_recomputes_and_a_progress_tick_does_not() {
    let edges = DownloadEdges::new();
    let downloading = model_row("r1", LLM_SPEC, DownloadStatus::Downloading, None);
    let completed = model_row("r1", LLM_SPEC, DownloadStatus::Completed, None);

    assert!(!edges.note(std::slice::from_ref(&downloading)));
    assert!(!edges.note(std::slice::from_ref(&downloading)));
    assert!(edges.note(std::slice::from_ref(&completed)));
    assert!(!edges.note(std::slice::from_ref(&completed)));
    assert!(edges.note(&[]));
    assert!(!edges.note(&[]));
}

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

#[tokio::test]
async fn a_notification_through_the_erased_seam_broadcasts_a_snapshot() {
    let env = env();
    let mut conn = env.frames.register("watcher");
    let notifier: Arc<dyn RequirementsNotifier> = Arc::new(env.requirements.clone());

    notifier.changed();

    let frames = frames_within(&mut conn, 1).await;
    assert_eq!(frames[0]["kind"], "requirements.snapshot");
    let required = frames[0]["required"].as_array().unwrap();
    assert_eq!(required.len(), 3);
    assert_eq!(frames[0]["missing"].as_array().unwrap().len(), 3);
    assert_eq!(required[0]["source"], LLM_SPEC);
}

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

    // Wait until the worker is parked inside `probe`, so the burst lands
    // while one recompute is genuinely in flight.
    env.requirements.changed();
    wait_until(|| env.downloads.probe_networks().len() == 1).await;

    for _ in 0..5 {
        env.requirements.changed();
    }
    gate.add_permits(1);

    let frames = frames_within(&mut conn, 2).await;
    assert_eq!(frames.len(), 2);
    no_frame_within(&mut conn, Duration::from_millis(200)).await;
    assert_eq!(
        env.downloads.probe_networks().len(),
        2,
        "six notifications, two recomputes",
    );
}

/// Poll a condition, bounded.
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

#[tokio::test]
async fn a_failed_recompute_broadcasts_nothing() {
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

/// A `Greetings` for the routes that never touch it.
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

#[tokio::test]
async fn the_download_route_ensures_each_missing_group_once_and_installs_the_binaries() {
    let env = env();
    with_images(&env).await;
    let app = app(&env, no_greetings(&env));

    let (status, body) = call(&app, post("/api/v1/requirements/download", json!({}))).await;
    assert_eq!(status, 200);

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

#[tokio::test]
async fn one_failing_group_does_not_starve_the_others() {
    let env = env_deno(
        FakeDownloads {
            enqueue_fails: vec!["llm".to_string()],
            ..Default::default()
        },
        false,
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

#[tokio::test]
async fn a_download_with_no_missing_binary_never_calls_the_installer() {
    let env = env_deno(
        FakeDownloads {
            present: vec![
                LLM_SPEC.to_string(),
                MMPROJ_SPEC.to_string(),
                EMBED_MODEL_FILE.to_string(),
            ],
            ..Default::default()
        },
        true,
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

/// A `Chat` over this context with a scripted provider, so a turn needs no model.
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

/// A `Greetings` over this env's chat, on fast timing.
fn greetings_with(env: &Env, probe: Arc<dyn crate::chat::LoadingProbe>) -> super::Greetings {
    super::Greetings::with_timing(chat_for(&env.ctx), probe, fast_timing())
}

/// A `Greetings` that never waits for a model.
fn ready_greetings(env: &Env) -> super::Greetings {
    greetings_with(env, Arc::new(NeverLoading))
}

#[tokio::test]
async fn a_second_greeting_inside_the_window_is_suppressed_and_says_why() {
    let env = env();
    let app = app(&env, ready_greetings(&env));

    // 0 floors to 1s, the smallest window a test can wait out.
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

    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (_, body) = call(&app, post("/api/v1/greetings/run", short())).await;
    assert_eq!(body, json!({ "ran": true }));

    // Enough to reopen a 1s window, nowhere near the 60s default.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (_, body) = call(&app, post("/api/v1/greetings/run", json!({}))).await;
    assert_eq!(body, json!({ "ran": false, "reason": "recent" }));
}

#[tokio::test]
async fn a_client_cannot_ask_for_a_zero_length_cooldown() {
    let env = env();
    let app = app(&env, ready_greetings(&env));

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

#[tokio::test]
async fn one_client_greeting_does_not_suppress_another() {
    let env = env();
    let greetings = ready_greetings(&env);

    assert!(greetings.run("client-a", Default::default()).ran);
    assert!(!greetings.run("client-a", Default::default()).ran);
    assert!(greetings.run("client-b", Default::default()).ran);
}

#[tokio::test]
async fn an_unknown_field_in_the_greeting_body_is_refused() {
    let env = env();
    let app = app(&env, ready_greetings(&env));

    let (status, body) = call(
        &app,
        post("/api/v1/greetings/run", json!({ "sessionTitel": "typo" })),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["code"], "validation_error");
}

#[tokio::test]
async fn a_greeting_opens_an_automated_session_and_announces_it() {
    let env = env();
    let app = app(&env, ready_greetings(&env));
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

/// The session store [`chat_for`]'s `Chat` writes through.
fn chat_store(ctx: &AppContext) -> crate::sessions::SessionsStore {
    crate::sessions::SessionsStore::new(Arc::clone(&ctx.paths))
}

#[tokio::test]
async fn a_greeting_waits_for_the_model_and_then_runs() {
    let env = env();
    let loading = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let greetings = greetings_with(&env, Arc::new(GatedLoading(Arc::clone(&loading))));
    let mut conn = env.frames.register(CLIENT);

    assert!(greetings.run(CLIENT, Default::default()).ran);
    no_frame_within(&mut conn, Duration::from_millis(100)).await;

    loading.store(false, Ordering::SeqCst);
    let created = frames_within(&mut conn, 1).await.remove(0);
    assert_eq!(created["kind"], "session.created");
}

#[tokio::test]
async fn a_model_that_never_loads_stops_holding_the_greeting_past_the_budget() {
    let env = env();
    let always = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let greetings = greetings_with(&env, Arc::new(GatedLoading(always)));
    let mut conn = env.frames.register(CLIENT);

    assert!(greetings.run(CLIENT, Default::default()).ran);
    let created = frames_within(&mut conn, 1).await.remove(0);
    assert_eq!(created["kind"], "session.created");
}

fn map(value: Value) -> crate::settings::SettingsRecord {
    value.as_object().unwrap().clone()
}
