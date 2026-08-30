//! Embeddings: L2-normalised vectors from the local engine (default) or, when
//! `llm.provider` is `external` and a Relevance Model is set, that provider's
//! OpenAI-shaped `/embeddings`. With no source configured `embed` fails with
//! `server_unavailable` and every caller degrades quietly.
//!
//! The active model's identity is folded into every stored
//! `embedding_source_hash`, so respelling the SAME model marks a real install's
//! whole vector store stale with no error anywhere. [`EMBED_REPO`] is that
//! string and a test pins it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::{AppError, ErrorCode, Result};
use crate::external::resolve_external_api_key;
use crate::secrets::Vault;
use crate::settings::access::str_setting;
use crate::settings::{Settings, SettingsRecord};

use super::relevance::embed_source_hash;

/// The local embedding model's identity, exactly as `@loca/shared` names it.
/// Do not reformat: it is hashed into every stored `embedding_source_hash`.
pub const EMBED_REPO: &str = "@second-state/All-MiniLM-L6-v2-Embedding-GGUF/main";

/// Stops a wedged-but-listening endpoint from pinning the caller forever.
const EMBED_TIMEOUT_MS: u64 = 30_000;

/// A local, in-process embedding engine.
#[async_trait]
pub trait LocalEmbedder: Send + Sync {
    /// The identity of the model this engine embeds with. Override only for a
    /// genuinely different model; getting it wrong is silent.
    fn model_id(&self) -> &str {
        EMBED_REPO
    }

    /// Whether the model is present and loadable. Must be cheap and load
    /// nothing.
    async fn is_available(&self) -> bool;

    /// One vector per input, in input order, unnormalised is fine.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Whether this is a real engine rather than [`NoLocalEmbedder`]. Exists so
    /// a boot-seam test can tell "wired but model not downloaded" (where
    /// `is_available` is false) from "not wired at all".
    fn is_local_engine(&self) -> bool {
        true
    }
}

/// The default local engine: none.
pub struct NoLocalEmbedder;

#[async_trait]
impl LocalEmbedder for NoLocalEmbedder {
    async fn is_available(&self) -> bool {
        false
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(AppError::new(
            ErrorCode::ServerUnavailable,
            "no local embedding engine on this core",
        ))
    }

    fn is_local_engine(&self) -> bool {
        false
    }
}

/// The embedding service. Clones share one identity cache and one HTTP client.
#[derive(Clone)]
pub struct Embedding {
    inner: Arc<Inner>,
}

struct Inner {
    settings: Settings,
    vault: Vault,
    local: Arc<dyn LocalEmbedder>,
    client: reqwest::Client,
    /// Refreshed at boot and on every settings change, so `source_hash` stays
    /// synchronous.
    active_model_id: Mutex<String>,
}

impl Embedding {
    pub fn new(settings: Settings, vault: Vault, local: Arc<dyn LocalEmbedder>) -> Self {
        let active_model_id = Mutex::new(local.model_id().to_string());
        Self {
            inner: Arc::new(Inner {
                settings,
                vault,
                local,
                client: reqwest::Client::new(),
                active_model_id,
            }),
        }
    }

    /// With no local engine.
    pub fn unwired(settings: Settings, vault: Vault) -> Self {
        Self::new(settings, vault, Arc::new(NoLocalEmbedder))
    }

    /// Whether this service got a real local engine (the boot seam's read port).
    pub fn has_local_engine(&self) -> bool {
        self.inner.local.is_local_engine()
    }

    /// Resolve the active identity now and keep it current as settings change.
    pub async fn start(&self) -> Result<()> {
        let settings = self.inner.settings.load_resolved().await?;
        self.refresh_identity(&settings);
        let this = self.clone();
        let mut changes = self.inner.settings.subscribe();
        tokio::spawn(async move {
            loop {
                match changes.recv().await {
                    Ok(change) => this.refresh_identity(&change.merged),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if let Ok(settings) = this.inner.settings.load_resolved().await {
                            this.refresh_identity(&settings);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Ok(())
    }

    fn refresh_identity(&self, settings: &SettingsRecord) {
        let id = external_embed_model(settings)
            .unwrap_or_else(|| self.inner.local.model_id().to_string());
        *lock(&self.inner.active_model_id) = id;
    }

    /// The identity of the model embeddings are currently produced with.
    pub fn active_model_id(&self) -> String {
        lock(&self.inner.active_model_id).clone()
    }

    /// The staleness hash for `text` under the active model.
    pub fn source_hash(&self, text: &str) -> String {
        embed_source_hash(&self.active_model_id(), text)
    }

    /// Whether embeddings can be produced right now.
    pub async fn is_ready(&self) -> bool {
        match self.inner.settings.load_resolved().await {
            Ok(settings) => matches!(self.resolve(&settings).await, Ok(Some(_))),
            Err(_) => false,
        }
    }

    /// L2-normalised vectors, one per input, in input order. Empty input
    /// returns empty output without touching any endpoint.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let settings = self.inner.settings.load_resolved().await?;
        let Some(endpoint) = self.resolve(&settings).await? else {
            return Err(AppError::new(
                ErrorCode::ServerUnavailable,
                "embeddings are not available (no local model, and no external Relevance Model configured)",
            ));
        };
        let raw = match endpoint {
            Endpoint::Local => self.inner.local.embed(texts).await?,
            Endpoint::External {
                url,
                model,
                api_key,
            } => self.embed_external(&url, &model, &api_key, texts).await?,
        };
        finish(raw, texts.len())
    }

    /// Embed one text paired with its staleness hash. `None` when the source
    /// yields nothing.
    pub async fn embed_with_hash(&self, text: &str) -> Result<Option<EmbeddedText>> {
        let vectors = self.embed(std::slice::from_ref(&text.to_string())).await?;
        Ok(vectors.into_iter().next().map(|vector| EmbeddedText {
            vector,
            source_hash: self.source_hash(text),
        }))
    }

    async fn embed_external(
        &self,
        url: &str,
        model: &str,
        api_key: &str,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>> {
        let mut request = self
            .inner
            .client
            .post(url)
            .timeout(Duration::from_millis(EMBED_TIMEOUT_MS))
            .json(&EmbedRequest {
                model,
                input: texts,
            });
        if !api_key.is_empty() {
            request = request.bearer_auth(api_key);
        }
        let response = request.send().await.map_err(|err| {
            AppError::new(
                ErrorCode::ServerUnavailable,
                format!("embeddings endpoint not reachable at {url}: {err}"),
            )
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::new(
                ErrorCode::ProviderError,
                format!("embeddings HTTP {} from {url}", status.as_u16()),
            ));
        }
        let body: EmbedResponse = response.json().await.map_err(|err| {
            AppError::new(
                ErrorCode::ProviderError,
                format!("embeddings returned a malformed body from {url}: {err}"),
            )
        })?;
        // Sort by `index` rather than trusting arrival order.
        let mut data = body.data.unwrap_or_default();
        data.sort_by_key(|d| d.index.unwrap_or(0));
        Ok(data
            .into_iter()
            .map(|d| {
                d.embedding
                    .unwrap_or_default()
                    .into_iter()
                    .map(|v| v as f32)
                    .collect()
            })
            .collect())
    }

    /// Where to embed, or `None` when not configured or not available.
    async fn resolve(&self, settings: &SettingsRecord) -> Result<Option<Endpoint>> {
        if let Some(model) = external_embed_model(settings) {
            let base_url = str_setting(settings, "llm.external.baseUrl", "")
                .trim()
                .to_string();
            if base_url.is_empty() {
                return Ok(None);
            }
            let api_key = resolve_external_api_key(
                settings,
                &self.inner.vault,
                "llm.external.apiKey",
                &base_url,
            )
            .await?;
            if api_key.is_empty() {
                return Ok(None);
            }
            return Ok(Some(Endpoint::External {
                url: format!("{}/embeddings", base_url.trim_end_matches('/')),
                model,
                api_key,
            }));
        }
        Ok(self
            .inner
            .local
            .is_available()
            .await
            .then_some(Endpoint::Local))
    }
}

/// A vector and the hash of the text plus model it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedText {
    pub vector: Vec<f32>,
    pub source_hash: String,
}

enum Endpoint {
    Local,
    External {
        url: String,
        model: String,
        api_key: String,
    },
}

#[derive(serde::Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Option<Vec<EmbedDatum>>,
}

#[derive(Deserialize)]
struct EmbedDatum {
    index: Option<i64>,
    embedding: Option<Vec<f64>>,
}

/// The active Relevance Model, or `None` when not in external mode or blank.
fn external_embed_model(settings: &SettingsRecord) -> Option<String> {
    if str_setting(settings, "llm.provider", "local") != "external" {
        return None;
    }
    let model = str_setting(settings, "llm.external.embedModel", "")
        .trim()
        .to_string();
    (!model.is_empty()).then_some(model)
}

/// Check a batch for internal consistency (count, uniform non-zero width) and
/// normalise it.
fn finish(vectors: Vec<Vec<f32>>, expected: usize) -> Result<Vec<Vec<f32>>> {
    if vectors.len() != expected {
        return Err(AppError::new(
            ErrorCode::ProviderError,
            format!(
                "embeddings returned {} vectors for {expected} inputs",
                vectors.len()
            ),
        ));
    }
    let mut dim = None;
    let mut out = Vec::with_capacity(vectors.len());
    for mut vector in vectors {
        let width = vector.len();
        let expected_width = *dim.get_or_insert(width);
        if width == 0 || width != expected_width {
            return Err(AppError::new(
                ErrorCode::ProviderError,
                format!(
                    "embeddings returned an inconsistent vector width ({width} vs {expected_width})"
                ),
            ));
        }
        l2_normalize(&mut vector);
        out.push(vector);
    }
    Ok(out)
}

/// L2-normalise in place. Accumulation and division are `f64` with a single
/// rounding to `f32` on store; `f32` maths gives a different (still unit)
/// vector that changes rankings. The fixture pins the exact bits.
pub fn l2_normalize(v: &mut [f32]) {
    let mut sum = 0.0f64;
    for x in v.iter() {
        sum += f64::from(*x) * f64::from(*x);
    }
    let norm = sum.sqrt();
    // `> 0.0`: a zero vector is left alone, and a NaN norm skips too.
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x = (f64::from(*x) / norm) as f32;
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
