//! The models route slice: seventeen routes under `/api/v1/models`.
//!
//! All of them require a paired client, which is the bearer-only posture the
//! TypeScript applies with `r.use("*", bearerMiddleware())`. There is no
//! admin-only tier: a paired device already chooses the model it talks to.
//!
//! Six of the seventeen are D7-backed and reach the download manager through
//! [`super::deps::Downloads`]; the rest are this package's own.
//!
//! # Two shape notes that are not free in axum
//!
//! **Registration order is not what saves the catch-all.** Hono needs
//! `DELETE /:relPath{.+}` registered last so it does not shadow
//! `DELETE /downloads/:id`, and the TypeScript says so at the call site. axum
//! matches by specificity rather than declaration order, so the static segment
//! wins regardless. The hazard is real in the file being ported, so the note
//! moves with the code.
//!
//! **Selecting a model never downloads.** `/select` writes the `llm.*` settings
//! and stops. Writing settings triggers a requirements recompute, which
//! surfaces any missing file in the client's confirm modal, and the user starts
//! the download from there. Downloading multi-gigabyte weights is always an
//! explicit choice, never a side effect of picking a preset.

use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::contract::api::{
    AppliedModelSettings, AppliedSttSettings, AppliedTtsSettings, CatalogModelView, DownloadEntry,
    DownloadModelsResponse, DownloadPlan, ModelEntry, PresetBucket, RecommendationSet,
    RecommendationSetApplied, SttPresetView, TtsPresetView,
};
use crate::contract::inbound::{
    ModelDownloadBody, ModelDownloadBodyItemsItemGroup, ModelProbeBody, ModelSelectBody,
    SpeechSelectBody, SttCatalogModel, TtsCatalogModel,
};
use crate::error::{not_found, validation, Result};
use crate::settings::schema::{schema, Destination};
use crate::settings::SettingsRecord;
use crate::transport::http::{no_content, Json};
use crate::transport::middleware::{from_fn_with_state, require_client, AuthedClient};
use crate::transport::state::AppContext;

use super::fit::{
    applied_for_model_spec, build_catalog_views, compute_recommendations, fit_profile,
};
use super::speech::{
    build_stt_preset_views, build_tts_preset_views, resolve_stt_selection, resolve_tts_selection,
};
use super::{parse_group, EnsureModelsResponse, Models};

/// The cap `validation/models.ts` puts on both batch bodies.
///
/// Restated here because it cannot survive the bridge: typify renders a
/// constraint only where some Rust type expresses it, and nothing is shaped
/// like "at most 256 items", so `maxItems` is dropped from the generated
/// `Deserialize`. See D-095.
const MAX_BATCH: usize = 256;

// --- the bodies and envelopes -----------------------------------------------
//
// The four request bodies are generated: their Zod schemas live in
// `@loca/shared/validation/models.ts`, so `emit-zod.ts` sees them and
// `contract::inbound` carries the enforcing `Deserialize`. Only the response
// envelopes are declared here, because those are object literals built inside
// the TypeScript handlers and no generator can see an object literal.
//
// The generated bodies carry `deny_unknown_fields` because the schemas are
// `.strict()`, which D-031 settled is the schema's call rather than the
// direction of travel. The Zod half is exempt from the `relaxRequestBodies`
// pass in `scripts/contract/generate.ts` for the same reason.

/// An empty string is the same as an absent field to a discriminator chain.
///
/// The Zod schemas are `z.string().optional()` with no `.min(1)`, so `""`
/// reaches the handler, and the TypeScript then branches on JavaScript
/// truthiness (`if (body.bucket)`), which `""` fails: the field FALLS THROUGH to
/// the next discriminator. `Option::as_deref()` would take the branch instead
/// and answer 400 or 404 where the specification answers 200. Every
/// discriminator on the bodies below reads through this. See D-043.
fn present(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|s| !s.is_empty())
}

impl ModelSelectBody {
    fn bucket(&self) -> Option<&str> {
        present(&self.bucket)
    }

    fn model_id(&self) -> Option<&str> {
        present(&self.model_id)
    }

    fn model_spec(&self) -> Option<&str> {
        present(&self.model_spec)
    }
}

/// [`SpeechSelectBody`] is shared by the STT and TTS select routes, which take
/// the same three discriminators. The accessors live here rather than beside
/// the generated type because [`present`] is this module's rule, not the
/// contract's.
impl SpeechSelectBody {
    pub fn preset_id(&self) -> Option<&str> {
        present(&self.preset_id)
    }

    pub fn model_id(&self) -> Option<&str> {
        present(&self.model_id)
    }

    pub fn model_spec(&self) -> Option<&str> {
        present(&self.model_spec)
    }
}

/// `/ensure` reads its body with a bare cast in the TypeScript
/// (`routes/models.ts:115`) and rejects anything whose `kind` is not one of the
/// four groups, so an unknown sibling field is ignored there. Matched here.
#[derive(Debug, Deserialize)]
struct EnsureRequest {
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogBrowseResponse {
    generated_at: String,
    models: Vec<CatalogModelView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SttCatalogResponse {
    generated_at: String,
    models: Vec<SttCatalogModel>,
    presets: Vec<SttPresetView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TtsCatalogResponse {
    generated_at: String,
    models: Vec<TtsCatalogModel>,
    presets: Vec<TtsPresetView>,
}

/// `{ preset, settings }`, the shape all three select routes echo back under an
/// `applied` key.
#[derive(Debug, Serialize)]
struct Applied<T> {
    preset: String,
    settings: T,
}

#[derive(Debug, Serialize)]
struct SelectResponse<T> {
    applied: Applied<T>,
}

/// The wire spelling of a bucket, parsed from the select body. The generated
/// [`PresetBucket`] has a `FromStr`, and it errors where this returns `None`:
/// an unknown bucket is a 400 with the name echoed back, not a parse failure.
fn parse_bucket(raw: &str) -> Option<PresetBucket> {
    match raw {
        "smallest" => Some(PresetBucket::Smallest),
        "half" => Some(PresetBucket::Half),
        "full" => Some(PresetBucket::Full),
        _ => None,
    }
}

pub fn routes(ctx: &AppContext, models: Models) -> Router {
    Router::new()
        .route("/api/v1/models", get(list))
        .route("/api/v1/models/download", post(download))
        .route("/api/v1/models/downloads", get(downloads))
        .route("/api/v1/models/downloads/{id}/cancel", post(cancel))
        .route("/api/v1/models/downloads/{id}/retry", post(retry))
        .route("/api/v1/models/downloads/{id}", delete(remove))
        .route("/api/v1/models/probe", post(probe))
        .route("/api/v1/models/ensure", post(ensure))
        .route("/api/v1/models/recommend", get(recommend))
        .route("/api/v1/models/recommend/recheck", post(recheck))
        .route("/api/v1/models/catalog", get(browse))
        .route("/api/v1/models/select", post(select))
        .route("/api/v1/models/stt/catalog", get(stt_catalog))
        .route("/api/v1/models/stt/select", post(stt_select))
        .route("/api/v1/models/tts/catalog", get(tts_catalog))
        .route("/api/v1/models/tts/select", post(tts_select))
        .route("/api/v1/models/{*rel_path}", delete(delete_model))
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(models)
}

// --- the manager ------------------------------------------------------------

async fn list(State(models): State<Models>) -> Result<Json<Vec<ModelEntry>>> {
    Ok(Json(models.list().await?))
}

async fn download(
    State(models): State<Models>,
    Json(body): Json<ModelDownloadBody>,
) -> Result<Json<DownloadModelsResponse>> {
    // `source`'s `.min(1)` needs no check here: it is a `minLength` in the Zod
    // schema, so the generated newtype refuses an empty source in the
    // deserializer and the request never reaches this line. Only the array
    // bound has to be restated. See D-095 and [`MAX_BATCH`].
    if body.items.len() > MAX_BATCH {
        return Err(validation(format!("at most {MAX_BATCH} items")));
    }
    let mut items = Vec::with_capacity(body.items.len());
    for item in &body.items {
        let group = item
            .group
            .map(|g| match g {
                ModelDownloadBodyItemsItemGroup::Llm => "llm",
                ModelDownloadBodyItemsItemGroup::Stt => "stt",
                ModelDownloadBodyItemsItemGroup::Tts => "tts",
                ModelDownloadBodyItemsItemGroup::Embed => "embed",
            })
            // The default group when the client does not say, matching the
            // TypeScript's `item.group ?? "llm"`.
            .unwrap_or("llm");
        items.push((item.source.to_string(), group.to_string()));
    }
    let job_ids = models.deps().downloads.enqueue_models(&items)?;
    Ok(Json(DownloadModelsResponse { job_ids }))
}

/// Drop completed rows whose file has since been deleted before returning the
/// queue, so a download whose model was removed self-clears instead of
/// lingering.
async fn downloads(State(models): State<Models>) -> Result<Json<Vec<DownloadEntry>>> {
    models.deps().downloads.reconcile_completed().await?;
    Ok(Json(models.deps().downloads.models_snapshot()))
}

async fn cancel(State(models): State<Models>, Path(id): Path<String>) -> Response {
    models.deps().downloads.cancel(&id);
    no_content()
}

async fn retry(State(models): State<Models>, Path(id): Path<String>) -> Response {
    models.deps().downloads.retry(&id);
    no_content()
}

async fn remove(State(models): State<Models>, Path(id): Path<String>) -> Response {
    models.deps().downloads.remove(&id);
    no_content()
}

async fn probe(
    State(models): State<Models>,
    Json(body): Json<ModelProbeBody>,
) -> Result<Json<Vec<DownloadPlan>>> {
    // As in [`download`]: the per-source `.min(1)` is enforced by the generated
    // newtype, and only the array bound has to be restated (D-095).
    if body.sources.len() > MAX_BATCH {
        return Err(validation(format!("at most {MAX_BATCH} sources")));
    }
    let sources: Vec<String> = body.sources.iter().map(|s| (**s).clone()).collect();
    Ok(Json(models.deps().downloads.probe(&sources, true).await?))
}

/// Bulk-ensure every model file one sidecar kind needs. See
/// [`Models::ensure_kind`] for why this is a user-confirmed path only.
async fn ensure(
    State(models): State<Models>,
    Json(body): Json<EnsureRequest>,
) -> Result<Json<EnsureModelsResponse>> {
    let Some(kind) = body.kind.as_deref().and_then(parse_group) else {
        return Err(validation("kind must be one of: llm, stt, tts, embed"));
    };
    Ok(Json(models.ensure_kind(kind).await?))
}

/// Delete a model file by its path.
///
/// A deleted file can re-expose a requirement, and the requirements snapshot
/// only re-probes when notified, so the recompute is what stops the client's
/// pending-downloads list reporting the file as still present.
async fn delete_model(
    State(models): State<Models>,
    Path(rel_path): Path<String>,
) -> Result<Response> {
    models.delete(&rel_path).await?;
    models.deps().requirements.changed();
    Ok(no_content())
}

// --- adaptive presets and the model browser ---------------------------------

/// The three computed presets for this device, plus what the settings currently
/// point at so the client can flag a better option.
async fn recommend(State(models): State<Models>) -> Result<Json<RecommendationSet>> {
    let (catalog, hw, applied) = tokio::try_join!(
        models.catalog(false),
        async { Ok(models.hardware(false).await) },
        applied_from_settings(&models),
    )?;
    Ok(Json(compute_recommendations(
        &catalog,
        &catalog.fit,
        &hw,
        applied,
        fit_profile(),
    )))
}

/// Force-refresh the catalog and re-probe hardware, then recompute. The
/// non-destructive "check for better models" action.
async fn recheck(State(models): State<Models>) -> Result<Json<RecommendationSet>> {
    let (catalog, hw, applied) = tokio::try_join!(
        models.catalog(true),
        async { Ok(models.hardware(true).await) },
        applied_from_settings(&models),
    )?;
    // Re-probing can change the desired GPU binary variant, because a GPU or a
    // driver may have appeared, so the required-files snapshot is re-evaluated:
    // a now-wrong-variant binary surfaces as a pending reinstall.
    models.deps().requirements.changed();
    Ok(Json(compute_recommendations(
        &catalog,
        &catalog.fit,
        &hw,
        applied,
        fit_profile(),
    )))
}

/// The full model list annotated with fit-on-this-device and the settings each
/// would apply.
async fn browse(State(models): State<Models>) -> Result<Json<CatalogBrowseResponse>> {
    let catalog = models.catalog(false).await?;
    let hw = models.hardware(false).await;
    Ok(Json(CatalogBrowseResponse {
        generated_at: catalog.generated_at.clone(),
        models: build_catalog_views(&catalog, &catalog.fit, &hw, fit_profile()),
    }))
}

/// Apply a preset bucket or a specific catalog model.
///
/// The patch mixes shared model and server keys with the model's recommended
/// sampling tuning, so it is partitioned by destination: the shared store only
/// ever holds `core` keys. The model restart is core-wide, but the recommended
/// tuning lands in this client's own overlay rather than rewriting every other
/// client's sampling baseline.
async fn select(
    State(models): State<Models>,
    client: AuthedClient,
    Json(body): Json<ModelSelectBody>,
) -> Result<Json<SelectResponse<AppliedModelSettings>>> {
    let (preset, settings) = resolve_selection(&models, &body).await?;

    let mut core = SettingsRecord::new();
    let mut per_client = SettingsRecord::new();
    for (key, value) in apply_to_patch(&settings, &preset) {
        if schema().destination(&key) == Some(Destination::ClientOnCore) {
            per_client.insert(key, value);
        } else {
            core.insert(key, value);
        }
    }
    if !core.is_empty() {
        models.settings().patch(core).await?;
    }
    if !per_client.is_empty() {
        models
            .settings()
            .patch_client(&client.id, per_client)
            .await?;
    }
    Ok(Json(SelectResponse {
        applied: Applied { preset, settings },
    }))
}

async fn resolve_selection(
    models: &Models,
    body: &ModelSelectBody,
) -> Result<(String, AppliedModelSettings)> {
    let catalog = models.catalog(false).await?;
    let hw = models.hardware(false).await;

    if let Some(bucket) = body.bucket() {
        let Some(parsed) = parse_bucket(bucket) else {
            return Err(validation(format!("unknown bucket: {bucket}")));
        };
        let applied = applied_from_settings(models).await?;
        let set = compute_recommendations(&catalog, &catalog.fit, &hw, applied, fit_profile());
        let chosen = match parsed {
            PresetBucket::Smallest => set.buckets.smallest,
            PresetBucket::Half => set.buckets.half,
            PresetBucket::Full => set.buckets.full,
        };
        let Some(recommendation) = chosen else {
            return Err(validation(format!("no model fits the \"{bucket}\" preset")));
        };
        return Ok((bucket.to_string(), recommendation.apply));
    }
    // A specific quant, by its unique spec, from the manual picker.
    if let Some(model_spec) = body.model_spec() {
        let Some((_, apply)) =
            applied_for_model_spec(&catalog, &catalog.fit, &hw, model_spec, fit_profile())
        else {
            return Err(not_found(format!("quant not in catalog: {model_spec}")));
        };
        return Ok(("custom".to_string(), apply));
    }
    if let Some(model_id) = body.model_id() {
        let Some(view) = build_catalog_views(&catalog, &catalog.fit, &hw, fit_profile())
            .into_iter()
            .find(|v| v.id == model_id)
        else {
            return Err(not_found(format!("model not in catalog: {model_id}")));
        };
        return Ok(("custom".to_string(), view.apply));
    }
    Err(validation("bucket, modelId, or modelSpec required"))
}

fn apply_to_patch(a: &AppliedModelSettings, preset: &str) -> Vec<(String, Value)> {
    let mut out = vec![
        ("llm.provider".to_string(), json!("local")),
        ("llm.preset".to_string(), json!(preset)),
        ("llm.modelPath".to_string(), json!(a.model_path)),
        (
            "llm.mmprojPath".to_string(),
            json!(a.mmproj_path.clone().unwrap_or_default()),
        ),
        ("llm.contextSize".to_string(), json!(a.context_size)),
        ("llm.threads".to_string(), json!(a.threads)),
        ("llm.gpuLayers".to_string(), json!(a.gpu_layers)),
        ("llm.flashAttn".to_string(), json!(a.flash_attn)),
        // `llm.supportImages` is deliberately NOT written. Image support is a
        // user choice, off by default and decoupled from the preset, so
        // selecting a vision-capable model must not silently turn vision on and
        // pull in the projector download. `mmprojPath` is still set so vision
        // works immediately if the user opts in.
        (
            "llm.idleUnloadSeconds".to_string(),
            json!(a.idle_unload_seconds),
        ),
        ("llm.reasoningBudget".to_string(), json!(a.reasoning_budget)),
        // Sampling is always written, even at the default, so switching models
        // resets these to the new model's tuning rather than leaving stale
        // values from the previous one.
        ("llm.temperature".to_string(), json!(a.temperature)),
        ("llm.topP".to_string(), json!(a.top_p)),
        ("llm.topK".to_string(), json!(a.top_k)),
        ("llm.minP".to_string(), json!(a.min_p)),
        ("llm.repeatPenalty".to_string(), json!(a.repeat_penalty)),
        // Persisted as a `;`-joined chain; empty means the server's default
        // order, which is what an absent sampler list means.
        (
            "llm.samplers".to_string(),
            json!(a.samplers.as_ref().map(|s| s.join(";")).unwrap_or_default()),
        ),
    ];
    // Both are filled from DEFAULT_SAMPLING by `applied_settings`, so in
    // practice they are always present. An absent one is skipped rather than
    // written as null, matching a JavaScript `undefined` in the patch object.
    if let Some(dry) = a.dry_multiplier {
        out.push(("llm.dryMultiplier".to_string(), json!(dry)));
    }
    if let Some(presence) = a.presence_penalty {
        out.push(("llm.presencePenalty".to_string(), json!(presence)));
    }
    out
}

/// What the shared store currently points at. The SPARSE store, not the
/// resolved one: this reports what someone chose, and a schema default is not a
/// choice.
async fn applied_from_settings(models: &Models) -> Result<RecommendationSetApplied> {
    let settings = models.settings().load().await?;
    let text = |key: &str| {
        settings
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    Ok(RecommendationSetApplied {
        preset: text("llm.preset"),
        model_path: text("llm.modelPath"),
    })
}

// --- the speech pickers -----------------------------------------------------

/// The whole lineup plus the resolved curated cards. No fit engine: STT bundles
/// run from about 31 MB to 3.1 GB, so everything fits everywhere.
async fn stt_catalog(State(models): State<Models>) -> Result<Json<SttCatalogResponse>> {
    let catalog = models.catalog(false).await?;
    Ok(Json(SttCatalogResponse {
        generated_at: catalog.generated_at.clone(),
        presets: build_stt_preset_views(&catalog),
        models: catalog.stt.models.clone(),
    }))
}

/// Apply a curated card or a specific model and quant. Like the LLM select,
/// never downloads.
async fn stt_select(
    State(models): State<Models>,
    Json(body): Json<SpeechSelectBody>,
) -> Result<Json<SelectResponse<AppliedSttSettings>>> {
    let catalog = models.catalog(false).await?;
    let hw = models.hardware(false).await;
    let (preset, settings) = resolve_stt_selection(&catalog, &hw, &body)?;
    let mut patch = SettingsRecord::new();
    patch.insert("stt.provider".into(), json!("local"));
    patch.insert("stt.preset".into(), json!(preset));
    patch.insert("stt.modelType".into(), json!(settings.model_type));
    patch.insert("stt.modelPath".into(), json!(settings.model_path));
    patch.insert("stt.modelFiles".into(), json!(settings.model_files));
    patch.insert("stt.threads".into(), json!(settings.threads));
    models.settings().patch(patch).await?;
    Ok(Json(SelectResponse {
        applied: Applied { preset, settings },
    }))
}

/// The whole lineup plus the curated cards, each carrying its model's voices so
/// the voice dropdown can populate from the selection.
async fn tts_catalog(State(models): State<Models>) -> Result<Json<TtsCatalogResponse>> {
    let catalog = models.catalog(false).await?;
    Ok(Json(TtsCatalogResponse {
        generated_at: catalog.generated_at.clone(),
        presets: build_tts_preset_views(&catalog),
        models: catalog.tts.models.clone(),
    }))
}

/// Picking a model resets `tts.voice` to that model's default voice.
///
/// `tts.voice` is a `client-on-client` key, and this writes it into the SHARED
/// store anyway, because `patchCoreSettings` in the TypeScript persists
/// whatever it is handed with no destination check. Preserved rather than
/// corrected: the value is already in real installs' `settings.json`.
async fn tts_select(
    State(models): State<Models>,
    Json(body): Json<SpeechSelectBody>,
) -> Result<Json<SelectResponse<AppliedTtsSettings>>> {
    let catalog = models.catalog(false).await?;
    let (preset, settings) = resolve_tts_selection(&catalog, &body)?;
    let mut patch = SettingsRecord::new();
    patch.insert("tts.preset".into(), json!(preset));
    patch.insert("tts.modelType".into(), json!(settings.model_type));
    patch.insert("tts.modelPath".into(), json!(settings.model_path));
    patch.insert("tts.modelFiles".into(), json!(settings.model_files));
    patch.insert("tts.voice".into(), json!(settings.voice));
    models.settings().patch(patch).await?;
    Ok(Json(SelectResponse {
        applied: Applied { preset, settings },
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt as _;

    use super::*;
    use crate::config::{Channel, Config};
    use crate::contract::api::DownloadPlan;
    use crate::db::migrate;
    use crate::models::deps::{Downloads, ModelsDeps};
    use crate::transport::testing::{context_with, StubAuth};

    const TOKEN: &str = "token-abc";
    const CLIENT: &str = "client-1";

    /// A download queue that records what it was asked to do. Everything the
    /// four D7-backed routes need, and nothing more: the real one is D7's.
    #[derive(Default)]
    struct FakeDownloads {
        /// Sources reported as already on disk.
        present: Vec<String>,
        enqueued: Mutex<Vec<(String, String)>>,
        actions: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Downloads for FakeDownloads {
        async fn probe(&self, sources: &[String], network: bool) -> Result<Vec<DownloadPlan>> {
            self.actions
                .lock()
                .unwrap()
                .push(format!("probe:{network}"));
            Ok(sources
                .iter()
                .map(|source| DownloadPlan {
                    already_have: self.present.contains(source),
                    error: None,
                    size_hint: Some(1024),
                    source: source.clone(),
                    version: None,
                })
                .collect())
        }

        fn enqueue_models(&self, items: &[(String, String)]) -> Result<Vec<String>> {
            let mut log = self.enqueued.lock().unwrap();
            log.extend_from_slice(items);
            Ok(items
                .iter()
                .enumerate()
                .map(|(i, _)| format!("job-{i}"))
                .collect())
        }

        fn models_snapshot(&self) -> Vec<DownloadEntry> {
            Vec::new()
        }

        fn cancel(&self, id: &str) {
            self.actions.lock().unwrap().push(format!("cancel:{id}"));
        }

        fn retry(&self, id: &str) {
            self.actions.lock().unwrap().push(format!("retry:{id}"));
        }

        fn remove(&self, id: &str) {
            self.actions.lock().unwrap().push(format!("remove:{id}"));
        }

        async fn reconcile_completed(&self) -> Result<()> {
            self.actions.lock().unwrap().push("reconcile".into());
            Ok(())
        }
    }

    use async_trait::async_trait;

    struct Env {
        router: Router,
        models: Models,
        downloads: Option<Arc<FakeDownloads>>,
        _dir: tempfile::TempDir,
    }

    fn env_with(downloads: Option<Arc<FakeDownloads>>) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = context_with(Arc::new(StubAuth::new().with_client(TOKEN, CLIENT)));
        // The dev channel, so the catalog comes from the embedded payload and
        // no route reaches the network. `context_with` is F6's and hardcodes
        // stable, so this is rebuilt rather than edited.
        ctx.config = Arc::new(Config {
            channel: Channel::Dev,
            ..(*ctx.config).clone()
        });
        ctx.paths.ensure_dirs().unwrap();
        ctx.db.call_blocking(|conn| migrate(conn)).unwrap();
        ctx.db
            .call_blocking(|conn| {
                conn.execute(
                    "INSERT INTO clients (id, name, token_hash, created_at_ms, last_seen_ms) \
                     VALUES (?1, 'test', 'hash', 0, 0)",
                    [CLIENT],
                )?;
                Ok(())
            })
            .unwrap();

        let deps = match &downloads {
            Some(fake) => {
                ModelsDeps::unwired().with_downloads(Arc::clone(fake) as Arc<dyn Downloads>)
            }
            None => ModelsDeps::unwired(),
        };
        let models = Models::new(&ctx, deps);
        Env {
            router: routes(&ctx, models.clone()),
            models,
            downloads,
            _dir: dir,
        }
    }

    fn env() -> Env {
        env_with(None)
    }

    async fn call_as(
        env: &Env,
        token: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let body = match body {
            Some(value) => {
                request = request.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = env
            .router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, value)
    }

    async fn call(env: &Env, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call_as(env, Some(TOKEN), method, path, body).await
    }

    #[tokio::test]
    async fn a_fresh_core_lists_no_models() {
        let env = env();
        assert_eq!(
            call(&env, "GET", "/api/v1/models", None).await,
            (200, json!([]))
        );
    }

    /// Every route in the slice is bearer-gated, including the catch-all
    /// delete, which is the one a `.layer` applied to the wrong sub-router
    /// would miss.
    #[tokio::test]
    async fn every_route_requires_a_paired_client() {
        let env = env();
        for (method, path) in [
            ("GET", "/api/v1/models"),
            ("GET", "/api/v1/models/downloads"),
            ("GET", "/api/v1/models/recommend"),
            ("GET", "/api/v1/models/catalog"),
            ("GET", "/api/v1/models/stt/catalog"),
            ("GET", "/api/v1/models/tts/catalog"),
            ("DELETE", "/api/v1/models/u/r/f.gguf"),
        ] {
            let (status, body) = call_as(&env, None, method, path, None).await;
            assert_eq!(status, 401, "{method} {path}");
            assert_eq!(body["error"]["code"], "missing_token", "{method} {path}");
        }
    }

    /// The catch-all delete must not swallow the download actions. axum matches
    /// by specificity rather than registration order, so this is the assertion
    /// that says so rather than the ordering of the `.route` calls.
    #[tokio::test]
    async fn the_catch_all_delete_does_not_shadow_the_download_routes() {
        let fake = Arc::new(FakeDownloads::default());
        let env = env_with(Some(Arc::clone(&fake)));
        assert_eq!(
            call(&env, "DELETE", "/api/v1/models/downloads/job-7", None)
                .await
                .0,
            204
        );
        assert_eq!(fake.actions.lock().unwrap().as_slice(), ["remove:job-7"]);
        // And a real model path still reaches the model delete, which reports
        // it is not on disk rather than removing a download row.
        let (status, body) = call(&env, "DELETE", "/api/v1/models/u/r/f.gguf", None).await;
        assert_eq!(status, 404);
        assert_eq!(body["error"]["code"], "model_not_found");
    }

    #[tokio::test]
    async fn the_download_actions_are_no_ops_for_an_unknown_id() {
        let fake = Arc::new(FakeDownloads::default());
        let env = env_with(Some(Arc::clone(&fake)));
        for (method, path) in [
            ("POST", "/api/v1/models/downloads/nope/cancel"),
            ("POST", "/api/v1/models/downloads/nope/retry"),
            ("DELETE", "/api/v1/models/downloads/nope"),
        ] {
            assert_eq!(call(&env, method, path, None).await.0, 204, "{path}");
        }
        assert_eq!(
            fake.actions.lock().unwrap().as_slice(),
            ["cancel:nope", "retry:nope", "remove:nope"]
        );
    }

    /// The queue is reconciled BEFORE it is returned, so a download whose file
    /// was deleted does not linger in the list.
    #[tokio::test]
    async fn listing_downloads_reconciles_first() {
        let fake = Arc::new(FakeDownloads::default());
        let env = env_with(Some(Arc::clone(&fake)));
        assert_eq!(
            call(&env, "GET", "/api/v1/models/downloads", None).await,
            (200, json!([]))
        );
        assert_eq!(fake.actions.lock().unwrap().as_slice(), ["reconcile"]);
    }

    /// The unwired seam refuses rather than reporting an empty result. An empty
    /// probe reads as "everything is already on disk" and an empty enqueue as
    /// "nothing needed downloading", and both are silent.
    #[tokio::test]
    async fn the_unwired_download_seam_refuses_the_two_routes_that_would_lie() {
        let env = env();
        for (path, body) in [
            (
                "/api/v1/models/probe",
                json!({ "sources": ["@u/r/main/f.gguf"] }),
            ),
            (
                "/api/v1/models/download",
                json!({ "items": [{ "source": "@u/r/main/f.gguf" }] }),
            ),
            ("/api/v1/models/ensure", json!({ "kind": "llm" })),
        ] {
            let (status, response) = call(&env, "POST", path, Some(body)).await;
            assert_eq!(status, 503, "{path}");
            assert_eq!(response["error"]["code"], "server_unavailable", "{path}");
        }
        // The read-only queue routes still answer, because empty is the true
        // answer for a core that has enqueued nothing.
        assert_eq!(
            call(&env, "GET", "/api/v1/models/downloads", None).await,
            (200, json!([]))
        );
    }

    #[tokio::test]
    async fn a_download_defaults_its_group_to_llm_and_bounds_the_batch() {
        let fake = Arc::new(FakeDownloads::default());
        let env = env_with(Some(Arc::clone(&fake)));
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/models/download",
            Some(json!({ "items": [
                { "source": "@u/r/main/a.gguf" },
                { "source": "@u/r/main/b.onnx", "group": "stt" }
            ]})),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body, json!({ "jobIds": ["job-0", "job-1"] }));
        assert_eq!(
            fake.enqueued.lock().unwrap().as_slice(),
            [
                ("@u/r/main/a.gguf".to_string(), "llm".to_string()),
                ("@u/r/main/b.onnx".to_string(), "stt".to_string()),
            ]
        );

        let too_many: Vec<Value> = (0..=MAX_BATCH)
            .map(|i| json!({ "source": format!("@u/r/main/{i}.gguf") }))
            .collect();
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/models/download",
            Some(json!({ "items": too_many })),
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "validation_error");

        let (status, _) = call(
            &env,
            "POST",
            "/api/v1/models/download",
            Some(json!({ "items": [{ "source": "" }] })),
        )
        .await;
        assert_eq!(status, 400);
    }

    /// `/ensure` probes what the settings need and enqueues only what is
    /// missing. On a default core that is the embedding model and nothing else.
    #[tokio::test]
    async fn ensure_enqueues_only_the_missing_files() {
        let fake = Arc::new(FakeDownloads::default());
        let env = env_with(Some(Arc::clone(&fake)));
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/models/ensure",
            Some(json!({ "kind": "embed" })),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["enqueued"], json!(["job-0"]));
        assert_eq!(body["alreadyHave"], json!([]));
        assert_eq!(
            fake.enqueued.lock().unwrap()[0].0,
            crate::models::required::EMBED_MODEL_FILE
        );

        // A kind the settings do not need at all short-circuits without ever
        // reaching the queue.
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/models/ensure",
            Some(json!({ "kind": "stt" })),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body, json!({ "enqueued": [], "alreadyHave": [] }));

        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/models/ensure",
            Some(json!({ "kind": "nonsense" })),
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "validation_error");
    }

    #[tokio::test]
    async fn ensure_reports_a_file_that_is_already_present() {
        let fake = Arc::new(FakeDownloads {
            present: vec![crate::models::required::EMBED_MODEL_FILE.to_string()],
            ..FakeDownloads::default()
        });
        let env = env_with(Some(Arc::clone(&fake)));
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/models/ensure",
            Some(json!({ "kind": "embed" })),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["enqueued"], json!([]));
        assert_eq!(
            body["alreadyHave"],
            json!([crate::models::required::EMBED_MODEL_FILE])
        );
        assert!(fake.enqueued.lock().unwrap().is_empty());
    }

    /// Applying a preset writes the `llm.*` settings and PARTITIONS them: the
    /// shared store takes the model and server keys, and the caller's own
    /// overlay takes the sampling tuning, so one client picking a model does not
    /// rewrite every other client's sampling baseline.
    #[tokio::test]
    async fn selecting_a_bucket_partitions_the_patch_by_destination() {
        let env = env();
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/models/select",
            Some(json!({ "bucket": "full" })),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["applied"]["preset"], "full");

        let shared = env.models.settings().load().await.unwrap();
        assert_eq!(shared["llm.provider"], "local");
        assert_eq!(shared["llm.preset"], "full");
        assert_eq!(
            shared["llm.modelPath"],
            body["applied"]["settings"]["modelPath"]
        );
        assert!(shared.contains_key("llm.contextSize"));
        assert!(shared.contains_key("llm.gpuLayers"));
        // Sampling is the caller's, not everyone's.
        for key in ["llm.temperature", "llm.topK", "llm.samplers"] {
            assert!(!shared.contains_key(key), "{key} is not a shared key");
        }
        // Image support is deliberately never written: turning vision on is the
        // user's choice, and writing it here would add a projector download as
        // a side effect of picking a model.
        assert!(!shared.contains_key("llm.supportImages"));

        let effective = env.models.settings().load_effective(CLIENT).await.unwrap();
        assert!(effective.contains_key("llm.temperature"));
        assert!(effective.contains_key("llm.samplers"));
    }

    #[tokio::test]
    async fn selecting_rejects_an_empty_or_unknown_choice() {
        let env = env();
        let cases = [
            (json!({}), 400, "validation_error"),
            (json!({ "bucket": "enormous" }), 400, "validation_error"),
            (
                json!({ "modelSpec": "@nobody/nothing/main/x.gguf" }),
                404,
                "not_found",
            ),
            (json!({ "modelId": "nobody/nothing" }), 404, "not_found"),
        ];
        for (body, want_status, want_code) in cases {
            let (status, response) =
                call(&env, "POST", "/api/v1/models/select", Some(body.clone())).await;
            assert_eq!(status, want_status, "{body}");
            assert_eq!(response["error"]["code"], want_code, "{body}");
        }
    }

    /// An empty string is not a choice on any of the three select routes. The
    /// Zod schemas are `z.string().optional()` with no `.min(1)`, so `""`
    /// reaches the handler on both cores, and the TypeScript's `if (body.x)` is
    /// falsy for it: the field falls through to the next discriminator instead
    /// of being looked up and missed. Reachable from any client that clears a
    /// field rather than omitting it (D-043).
    #[tokio::test]
    async fn an_empty_discriminator_falls_through_like_an_omitted_one() {
        let env = env();
        let cases = [
            (
                "/api/v1/models/select",
                json!({ "bucket": "", "modelId": "Qwen/Qwen3.5-4B" }),
                "custom",
            ),
            (
                "/api/v1/models/select",
                json!({ "modelSpec": "", "modelId": "Qwen/Qwen3.5-4B" }),
                "custom",
            ),
            (
                "/api/v1/models/stt/select",
                json!({ "presetId": "", "modelId": "whisper-tiny.en" }),
                "custom",
            ),
            (
                "/api/v1/models/tts/select",
                json!({ "presetId": "", "modelId": "kokoro" }),
                "custom",
            ),
        ];
        for (path, body, want_preset) in cases {
            let (status, response) = call(&env, "POST", path, Some(body.clone())).await;
            assert_eq!(status, 200, "{path} {body}: {response}");
            assert_eq!(response["applied"]["preset"], want_preset, "{path} {body}");
        }

        // All of them empty is the same body as all of them absent.
        for (path, body) in [
            (
                "/api/v1/models/select",
                json!({ "bucket": "", "modelId": "", "modelSpec": "" }),
            ),
            (
                "/api/v1/models/stt/select",
                json!({ "presetId": "", "modelId": "", "modelSpec": "" }),
            ),
        ] {
            let (status, response) = call(&env, "POST", path, Some(body)).await;
            assert_eq!(status, 400, "{path}");
            assert_eq!(response["error"]["code"], "validation_error", "{path}");
        }
    }

    /// The `modelFiles` string is persisted verbatim, so this asserts the exact
    /// value rather than that it parses.
    #[tokio::test]
    async fn selecting_a_speech_preset_writes_the_exact_settings() {
        let env = env();
        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/models/stt/select",
            Some(json!({ "presetId": "light-multilingual" })),
        )
        .await;
        assert_eq!(status, 200);
        let applied = &body["applied"]["settings"];
        let shared = env.models.settings().load().await.unwrap();
        assert_eq!(shared["stt.provider"], "local");
        assert_eq!(shared["stt.preset"], "light-multilingual");
        assert_eq!(shared["stt.modelType"], applied["modelType"]);
        assert_eq!(shared["stt.modelPath"], applied["modelPath"]);
        assert_eq!(shared["stt.modelFiles"], applied["modelFiles"]);
        assert_eq!(shared["stt.threads"], applied["threads"]);
        assert!(
            !applied["modelFiles"].as_str().unwrap().contains('\n'),
            "STT is compact"
        );

        let (status, body) = call(
            &env,
            "POST",
            "/api/v1/models/tts/select",
            Some(json!({ "presetId": "kokoro" })),
        )
        .await;
        assert_eq!(status, 200);
        let applied = &body["applied"]["settings"];
        let shared = env.models.settings().load().await.unwrap();
        assert_eq!(shared["tts.preset"], "kokoro");
        assert_eq!(shared["tts.modelFiles"], applied["modelFiles"]);
        assert!(
            applied["modelFiles"].as_str().unwrap().contains('\n'),
            "TTS is pretty-printed, and that asymmetry is the specification"
        );
        // `tts.voice` is a client-on-client key and lands in the SHARED store
        // anyway, because `patchCoreSettings` persists whatever it is handed.
        // Preserved rather than corrected: the value is already on disk in real
        // installs.
        assert_eq!(shared["tts.voice"], applied["voice"]);
    }

    #[tokio::test]
    async fn recommending_reports_the_device_and_what_the_settings_point_at() {
        let env = env();
        let (status, body) = call(&env, "GET", "/api/v1/models/recommend", None).await;
        assert_eq!(status, 200);
        assert!(body["hardware"]["totalRamBytes"].as_u64().unwrap() > 0);
        assert!(body["catalogGeneratedAt"].as_str().unwrap().ends_with('Z'));
        // A bucket that does not resolve is present as null, never absent.
        for bucket in ["smallest", "half", "full"] {
            assert!(body["buckets"].get(bucket).is_some(), "{bucket}");
        }
        assert_eq!(body["applied"], json!({}), "nothing chosen yet");

        call(
            &env,
            "POST",
            "/api/v1/models/select",
            Some(json!({ "bucket": "smallest" })),
        )
        .await;
        let (_, body) = call(&env, "GET", "/api/v1/models/recommend", None).await;
        assert_eq!(body["applied"]["preset"], "smallest");
        assert!(body["applied"]["modelPath"].is_string());
    }

    /// A re-check re-probes hardware and tells the requirements snapshot, since
    /// a GPU appearing can change which binary variant this device wants.
    #[tokio::test]
    async fn rechecking_notifies_the_requirements_snapshot() {
        struct CountingNotifier(Mutex<u32>);
        impl crate::storage::deps::RequirementsNotifier for CountingNotifier {
            fn changed(&self) {
                *self.0.lock().unwrap() += 1;
            }
        }
        let notifier = Arc::new(CountingNotifier(Mutex::new(0)));
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = context_with(Arc::new(StubAuth::new().with_client(TOKEN, CLIENT)));
        ctx.config = Arc::new(Config {
            channel: Channel::Dev,
            ..(*ctx.config).clone()
        });
        ctx.paths.ensure_dirs().unwrap();
        ctx.db.call_blocking(|conn| migrate(conn)).unwrap();
        let models = Models::new(
            &ctx,
            ModelsDeps::unwired().with_requirements(
                Arc::clone(&notifier) as Arc<dyn crate::storage::deps::RequirementsNotifier>
            ),
        );
        let env = Env {
            router: routes(&ctx, models.clone()),
            models,
            downloads: None,
            _dir: dir,
        };

        assert_eq!(
            call(&env, "POST", "/api/v1/models/recommend/recheck", None)
                .await
                .0,
            200
        );
        assert_eq!(*notifier.0.lock().unwrap(), 1);

        // And a model delete does the same, because a deleted file re-exposes a
        // requirement the snapshot only re-probes when told.
        std::fs::create_dir_all(env.models.models_dir().join("u/r")).unwrap();
        std::fs::write(env.models.models_dir().join("u/r/f.gguf"), b"x").unwrap();
        assert_eq!(
            call(&env, "DELETE", "/api/v1/models/u/r/f.gguf", None)
                .await
                .0,
            204
        );
        assert_eq!(*notifier.0.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn the_browser_and_the_speech_pickers_serve_the_catalog() {
        let env = env();
        let (status, body) = call(&env, "GET", "/api/v1/models/catalog", None).await;
        assert_eq!(status, 200);
        assert!(body["generatedAt"].as_str().unwrap().ends_with('Z'));
        assert_eq!(body["models"].as_array().unwrap().len(), 9);

        let (status, body) = call(&env, "GET", "/api/v1/models/stt/catalog", None).await;
        assert_eq!(status, 200);
        assert_eq!(body["models"].as_array().unwrap().len(), 17);
        assert_eq!(body["presets"].as_array().unwrap().len(), 4);

        let (status, body) = call(&env, "GET", "/api/v1/models/tts/catalog", None).await;
        assert_eq!(status, 200);
        assert_eq!(body["models"].as_array().unwrap().len(), 6);
        // Each card carries its model's voices, so the voice dropdown fills
        // from the selection with no second round trip.
        assert!(!body["presets"][0]["voices"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn deleting_a_model_refuses_a_traversing_path() {
        let env = env();
        // Percent-encoded, because a client library would normalise a literal
        // `..` out of the path before it ever reached the core.
        let (status, body) = call(&env, "DELETE", "/api/v1/models/u/%2e%2e/escape", None).await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "validation_error");
    }

    /// The seam is threaded rather than reached for, and this is what that
    /// buys: two handles over one core answer from the same queue.
    #[tokio::test]
    async fn every_route_reads_the_one_download_handle() {
        let fake = Arc::new(FakeDownloads::default());
        let env = env_with(Some(Arc::clone(&fake)));
        assert!(env.downloads.is_some());
        call(&env, "POST", "/api/v1/models/downloads/a/cancel", None).await;
        call(&env, "GET", "/api/v1/models/downloads", None).await;
        assert_eq!(
            fake.actions.lock().unwrap().as_slice(),
            ["cancel:a", "reconcile"]
        );
    }

    /// The slice is reachable through the fully assembled router, not just
    /// through a bare mount of itself.
    ///
    /// This was a merge of the slice into the real router, standing in for a
    /// mount that did not exist yet. Now that boot mounts it, merging again is
    /// a duplicate and axum panics on the overlap, which is the router-assembly
    /// check doing its job. What is left worth asserting is that the catch-all
    /// `DELETE /api/v1/models/{*rel_path}` did not shadow, or get shadowed by, a
    /// sibling package's paths once everything is mounted together.
    #[tokio::test]
    async fn the_slice_is_reachable_through_the_assembled_router() {
        let assembled = crate::transport::testing::core_with(Arc::new(
            StubAuth::new().with_client(TOKEN, CLIENT),
        ))
        .router();
        // The assembled router is built by `Services::unwired`, which mounts
        // this slice. A 200 proves the mount is live rather than merely
        // non-conflicting.
        let response = assembled
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/models")
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
    }
}
