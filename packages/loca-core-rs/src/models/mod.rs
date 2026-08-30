//! Model catalog, on-device fit, the model manager, and hardware introspection.
//!
//! [`Models`] is `Clone` over an `Arc` and `boot` builds exactly one, because of
//! the hardware cache: a probe spawns the vendor GPU tools on Linux and Windows
//! and loads the Vulkan loader, so a second instance repeats that per caller and
//! the two can disagree about what machine they are on after a re-probe.
//!
//! The rest of the core reaches in for [`ModelLocks`] and [`CatalogHashes`]
//! (both implemented below), [`Models::hardware`], [`Models::ensure_kind`],
//! [`Models::resolve_hf_path`], [`required::required_model_refs`] and
//! [`required::EMBED_MODEL_FILE`].

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
/// Declared here rather than generated, because it is a response envelope no
/// schema describes, and here rather than in `routes.rs` because
/// [`Models::ensure_kind`] is what returns it.
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

    /// The process-wide settings store, never a second instance: a second handle
    /// would carry its own file cache and write lock over one `settings.json`,
    /// which is exactly the lost update the settings lock exists to prevent.
    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    pub fn models_dir(&self) -> &std::path::Path {
        self.inner.paths.models_dir()
    }

    /// Detected hardware, cached. `force` re-probes.
    ///
    /// The probe is blocking (sysctls, and subprocesses on Linux and Windows), so
    /// it runs on the blocking pool.
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
    /// user-confirmed path: `POST /api/v1/models/ensure` or
    /// `POST /api/v1/requirements/download`. Never on a settings change or a
    /// preset select; those prompt the client's modal instead.
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

/// The storage seam: the model files the current settings pin in place, so the
/// storage view refuses to reclaim one that is in use.
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

/// The downloads seam: the sha256 the SIGNED catalog pins for one downloadable
/// file, so a transfer verifies its bytes against an anchor this core checked a
/// signature on rather than a header a proxy could have written.
///
/// The key is the `@user/repo/branch/file` spec exactly as the catalog writes
/// it, and nothing is normalised on either side: the derived forms (the
/// branch-less relative path, the absolute path under the models root) are what
/// a file is called ON DISK and none of them is a catalog key.
///
/// A catalog that will not load answers `None` deliberately. The transfer then
/// falls through to HuggingFace's own published sha256, which for a git-LFS
/// weight IS a content hash: a step down to a weaker anchor rather than to none,
/// where refusing outright would make a manifest-host outage stop every model
/// download on a core that can reach HuggingFace perfectly well. What it must
/// NOT do is answer from an unsigned source: the embedded dev payload is
/// unsigned by construction, so using it would pin a hash nobody signed.
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

    // Two specs from the shipped dev catalog, one per arm of the index (a family
    // variant's quant, and a speech model's file). Literals rather than reads off
    // the payload, so the pin is asserted rather than derived from the same data
    // the implementation walked.
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

    /// Held as a trait object, which is how the transfer holds it.
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

        // Next to the two positives on purpose: an implementation answering
        // `None` for everything passes a lone absence assertion.
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
    /// The derived forms are all reachable at the call site, and normalising here
    /// would either invent a pin for a path that is not a catalog key or, by
    /// case-folding, match a repo the catalog never signed.
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
        // reaching the network, the way an offline stable core does.
        let stable = models_on(Channel::Stable, "not-a-url");
        assert_eq!(stable.catalog_sha256(LLM_SPEC).await, None);

        // The same spec against a catalog that does load, so the assertion above
        // is about the catalog being unavailable, not the spec being unknown.
        assert_eq!(
            dev_models().catalog_sha256(LLM_SPEC).await.as_deref(),
            Some(LLM_SHA)
        );
    }
}
