//! Model catalog, on-device fit, and the model manager. Absorbs the hardware introspection helper.
//!
//! Work package **D6** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! # One handle, because the hardware probe is expensive and cacheable
//!
//! [`Models`] is `Clone` over an `Arc` and `boot` builds exactly one, the same
//! shape `SessionsStore` and `Auth` take. The reason is the hardware cache: a
//! probe spawns the vendor GPU tools on Linux and Windows and loads the Vulkan
//! loader, so a second instance repeats that per caller, and after a
//! `POST /recommend/recheck` forces a re-probe the two instances can disagree
//! about what machine they are on.
//!
//! # What the rest of the core reaches for here
//!
//! | consumer          | needs                                          |
//! | ----------------- | ---------------------------------------------- |
//! | D9 storage        | [`ModelLocks`], implemented below              |
//! | D7 downloads      | [`CatalogHashes`], implemented below           |
//! | D8 requirements   | [`required::required_model_refs`], [`Models::hardware`], [`Models::ensure_kind`] |
//! | D7 binaries       | [`Models::hardware`], for the GPU backend      |
//! | D14, N2, N3       | [`Models::resolve_hf_path`]                    |
//! | D15, N3           | [`required::EMBED_MODEL_FILE`]                 |

pub mod canonical;
pub mod catalog;
pub mod deps;
pub mod fit;
pub mod hardware;
pub mod manager;
pub mod required;
pub mod routes;
pub mod speech;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::config::Config;
use crate::contract::api::{HardwareInfo, ModelEntry, RequirementGroup};
use crate::contract::inbound::CatalogPayload;
use crate::downloads::deps::CatalogHashes;
use crate::error::Result;
use crate::paths::Paths;
use crate::settings::Settings;
use crate::storage::deps::ModelLocks;
use crate::transport::state::AppContext;

use deps::ModelsDeps;

/// What `POST /api/v1/models/ensure` answers with.
///
/// An object literal in `http/routes/models.ts` with no declaration in
/// `@loca/shared`, so neither contract generator can see it and there is no
/// generated type to point at (D-067). It lives here rather than in
/// `routes.rs` because [`Models::ensure_kind`] is what returns it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsureModelsResponse {
    /// Download queue-row ids, which cancel, retry and remove accept.
    pub enqueued: Vec<String>,
    pub already_have: Vec<String>,
}

struct Inner {
    config: Arc<Config>,
    paths: Arc<Paths>,
    settings: Settings,
    deps: ModelsDeps,
    /// Hardware does not change while the process runs, and probing it is not
    /// free. `POST /recommend/recheck` is the one thing that clears it, because
    /// a GPU or a driver appearing is exactly what that button is for.
    hardware: RwLock<Option<HardwareInfo>>,
}

/// The models service. Cheap to clone; every clone shares one hardware cache.
#[derive(Clone)]
pub struct Models {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Models {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Models").finish_non_exhaustive()
    }
}

impl Models {
    pub fn new(ctx: &AppContext, deps: ModelsDeps) -> Self {
        Self {
            inner: Arc::new(Inner {
                config: Arc::clone(&ctx.config),
                paths: Arc::clone(&ctx.paths),
                settings: ctx.settings.clone(),
                deps,
                hardware: RwLock::new(None),
            }),
        }
    }

    pub fn deps(&self) -> &ModelsDeps {
        &self.inner.deps
    }

    /// The process-wide settings store, never a second instance: three of the
    /// model routes write through it, and a second handle would carry its own
    /// file cache and its own write lock over one `settings.json`, which is the
    /// lost update `withSettingsLock` exists to prevent.
    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    pub fn models_dir(&self) -> &std::path::Path {
        self.inner.paths.models_dir()
    }

    /// Detected hardware, cached. `force` re-probes.
    ///
    /// The probe is blocking (sysctls, and subprocesses on Linux and Windows),
    /// so it runs on the blocking pool rather than stalling a runtime worker
    /// for as long as `nvidia-smi` takes to start.
    pub async fn hardware(&self, force: bool) -> HardwareInfo {
        if !force {
            if let Some(cached) = self.inner.hardware.read().await.clone() {
                return cached;
            }
        }
        let probed = tokio::task::spawn_blocking(hardware::detect)
            .await
            .unwrap_or_else(|_| hardware::detect());
        *self.inner.hardware.write().await = Some(probed.clone());
        probed
    }

    /// The catalog: compiled from source on dev, fetched and signature-verified
    /// otherwise. `force` skips the verified cache.
    pub async fn catalog(&self, force: bool) -> Result<CatalogPayload> {
        catalog::load(&self.inner.config, &self.inner.paths, force).await
    }

    /// An HF spec resolved to its absolute path under the models root.
    pub fn resolve_hf_path(&self, spec: &str) -> PathBuf {
        manager::resolve_hf_path(self.models_dir(), spec)
    }

    pub async fn list(&self) -> Result<Vec<ModelEntry>> {
        manager::list(self.models_dir()).await
    }

    pub async fn delete(&self, rel_path: &str) -> Result<()> {
        manager::delete(self.models_dir(), rel_path).await
    }

    /// Probe and enqueue whatever one sidecar kind is missing. Idempotent:
    /// files already on disk come back in `already_have`.
    ///
    /// This enqueues real multi-gigabyte downloads, so it runs only from a
    /// user-confirmed path: `POST /api/v1/models/ensure` (an explicit download
    /// button) or D8's `POST /api/v1/requirements/download` (the confirm
    /// modal). It is deliberately NOT called on a settings change or a preset
    /// select; those write settings, which prompts the client's modal, and the
    /// user starts the download from there.
    pub async fn ensure_kind(&self, kind: RequirementGroup) -> Result<EnsureModelsResponse> {
        let settings = self.inner.settings.load_resolved().await?;
        let sources = required::sources_for_kind(&settings, kind);
        if sources.is_empty() {
            return Ok(EnsureModelsResponse {
                enqueued: Vec::new(),
                already_have: Vec::new(),
            });
        }
        let plans = self.inner.deps.downloads.probe(&sources, true).await?;
        let mut already_have = Vec::new();
        let mut missing = Vec::new();
        for plan in plans {
            if plan.already_have {
                already_have.push(plan.source);
            } else {
                missing.push((plan.source, group_id(kind).to_string()));
            }
        }
        if missing.is_empty() {
            return Ok(EnsureModelsResponse {
                enqueued: Vec::new(),
                already_have,
            });
        }
        let enqueued = self.inner.deps.downloads.enqueue_models(&missing)?;
        Ok(EnsureModelsResponse {
            enqueued,
            already_have,
        })
    }
}

/// The wire spelling of a requirement group, which is what a download row's
/// `groupId` carries.
pub fn group_id(group: RequirementGroup) -> &'static str {
    match group {
        RequirementGroup::Llm => "llm",
        RequirementGroup::Stt => "stt",
        RequirementGroup::Tts => "tts",
        RequirementGroup::Embed => "embed",
    }
}

pub fn parse_group(raw: &str) -> Option<RequirementGroup> {
    match raw {
        "llm" => Some(RequirementGroup::Llm),
        "stt" => Some(RequirementGroup::Stt),
        "tts" => Some(RequirementGroup::Tts),
        "embed" => Some(RequirementGroup::Embed),
        _ => None,
    }
}

/// D9's seam: the model files the current settings pin in place, so the storage
/// view refuses to reclaim one that is in use.
///
/// This is the whole of `services/storage.ts:69`, which is why the seam is
/// filled here rather than in D9: both halves live in this module.
#[async_trait]
impl ModelLocks for Models {
    async fn required_model_paths(&self) -> Result<Vec<PathBuf>> {
        let settings = self.inner.settings.load_resolved().await?;
        Ok(required::required_model_refs(&settings)
            .into_iter()
            .map(|r| self.resolve_hf_path(&r.source))
            .collect())
    }
}

/// D7's seam: the sha256 the SIGNED catalog pins for one downloadable file, so
/// a transfer verifies its bytes against an anchor this core checked a signature
/// on rather than against a header a proxy could have written.
///
/// This is the whole of `models/catalog.ts:152`, and it is filled here because
/// the index it reads is this module's.
///
/// # The key is the source spec exactly as the catalog writes it
///
/// `build_sha256_index` keys by `modelSpec`, which is the
/// `@user/repo/branch/file` string, and D7 hands over `EnqueueSpec.source`,
/// which is the same string: every model download originates either from a
/// settings value copied out of the catalog or from a user-supplied spec in the
/// same form. So nothing is normalised on either side, and nothing may be: the
/// derived forms (the branch-less relative path `resolveHfPath` produces, the
/// absolute path under the models root) are what a file is called ON DISK, and
/// none of them is a catalog key.
///
/// # A catalog that will not load answers `None`, deliberately
///
/// Matching `modelCatalogSha256`, which swallows the load failure. The reachable
/// cause is a stable-channel core whose manifest host is down or whose cache was
/// never written, and the consequence is that the transfer falls through to
/// HuggingFace's own published sha256, which for a git-LFS weight IS a content
/// hash and is shape-checked before it is trusted. So this is a step down to a
/// weaker anchor rather than a step to no anchor, and the alternative (refusing
/// the download outright) would make a manifest-host outage stop every model
/// download on a core that can reach HuggingFace perfectly well.
///
/// What it must NOT do is answer from an unsigned source. In particular the
/// embedded dev payload is not a fallback for a stable core: it is unsigned by
/// construction, and using it would pin a hash the release process never signed.
#[async_trait]
impl CatalogHashes for Models {
    async fn catalog_sha256(&self, spec: &str) -> Option<String> {
        let payload = self.catalog(false).await.ok()?;
        catalog::build_sha256_index(&payload).remove(spec)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::Channel;
    use crate::transport::testing::context;

    // Two specs from the shipped dev catalog, one reaching the index by each of
    // its two arms (a family variant's quant, and a speech model's file). Both
    // are literals rather than reads off the payload, so the pin a download
    // verifies against is asserted rather than derived from the same data the
    // implementation walked. `deno task contract:check` is what keeps them
    // current: it fails on a stale `catalog.dev.json`.
    const LLM_SPEC: &str = "@unsloth/Qwen3.6-27B-GGUF/main/Qwen3.6-27B-Q8_0.gguf";
    const LLM_SHA: &str = "f93f517f38e696d35a1a7df2c0e3155a64f4c4dcd662107a146ae263f7fb14ce";
    const STT_SPEC: &str = "@csukuangfj/sherpa-onnx-whisper-tiny.en/main/tiny.en-encoder.int8.onnx";
    const STT_SHA: &str = "0ce578b827c94a961aacb8fa14b02f096504b337e5c94be37c36238cbe3e8bc6";

    fn models_on(channel: Channel, storage_base_url: &str) -> Models {
        let mut ctx = context();
        ctx.config = Arc::new(Config {
            channel,
            storage_base_url: storage_base_url.to_string(),
            ..(*ctx.config).clone()
        });
        Models::new(&ctx, deps::ModelsDeps::unwired())
    }

    /// The dev channel compiles the catalog in, so this reaches no network.
    fn dev_models() -> Models {
        models_on(Channel::Dev, "https://get.loca.rzkyif.com")
    }

    /// Held as a trait object, which is how D7's transfer holds it.
    #[tokio::test]
    async fn a_catalog_file_resolves_to_the_hash_the_signed_catalog_pins() {
        let catalog: Arc<dyn CatalogHashes> = Arc::new(dev_models());

        assert_eq!(
            catalog.catalog_sha256(LLM_SPEC).await.as_deref(),
            Some(LLM_SHA)
        );
        assert_eq!(
            catalog.catalog_sha256(STT_SPEC).await.as_deref(),
            Some(STT_SHA)
        );

        // The absence case lives next to the two positives on purpose: an
        // implementation that answered `None` for everything passes a lone
        // absence assertion, and those two are the instrument that would notice
        // (D-050). A spec the catalog does not list is a custom user pick, and
        // the transfer falls through to HuggingFace's hash for it.
        assert_eq!(
            catalog
                .catalog_sha256("@someone/custom-repo/main/model.gguf")
                .await,
            None
        );
    }

    /// The key is the spec as written, in both directions: the catalog's own
    /// string hits, and every form derived from it misses.
    ///
    /// Worth pinning because the derived forms are all reachable at the call
    /// site. Normalising here would either invent a pin for a path that is not a
    /// catalog key, or (case-folding) match a repo whose name differs only in
    /// case from the one the catalog signed.
    #[tokio::test]
    async fn the_lookup_key_is_the_spec_verbatim_and_never_a_form_derived_from_it() {
        let models = dev_models();
        assert_eq!(
            models.catalog_sha256(LLM_SPEC).await.as_deref(),
            Some(LLM_SHA),
            "the spec as the catalog writes it"
        );

        // What the same file is called on disk. `resolve_hf_path` drops the
        // branch segment, so neither the relative nor the absolute form is a key.
        let absolute = models.resolve_hf_path(LLM_SPEC);
        assert_eq!(
            models.catalog_sha256(absolute.to_str().unwrap()).await,
            None
        );
        assert_eq!(
            models
                .catalog_sha256("unsloth/Qwen3.6-27B-GGUF/Qwen3.6-27B-Q8_0.gguf")
                .await,
            None
        );

        // HuggingFace repo names are case-sensitive and this one is mixed case.
        assert_eq!(models.catalog_sha256(&LLM_SPEC.to_lowercase()).await, None);
        assert_eq!(models.catalog_sha256(&LLM_SPEC.to_uppercase()).await, None);

        // A different branch is a different file, even at the same path.
        assert_eq!(
            models
                .catalog_sha256(&LLM_SPEC.replace("/main/", "/master/"))
                .await,
            None
        );
    }

    /// A load failure is `None`, and specifically not the embedded dev payload:
    /// a stable core must never pin a hash off an unsigned catalog.
    #[tokio::test]
    async fn a_catalog_that_cannot_be_loaded_yields_no_pin_rather_than_an_unsigned_one() {
        // A manifest base the URL parser refuses, so the load fails without
        // reaching the network at all. The failure it produces is the one an
        // offline stable core with no written cache produces.
        let stable = models_on(Channel::Stable, "not-a-url");
        assert_eq!(stable.catalog_sha256(LLM_SPEC).await, None);

        // The same spec through the same code path against a catalog that does
        // load, so the assertion above is about the catalog being unavailable
        // rather than about the spec being unknown.
        assert_eq!(
            dev_models().catalog_sha256(LLM_SPEC).await.as_deref(),
            Some(LLM_SHA)
        );
    }
}
