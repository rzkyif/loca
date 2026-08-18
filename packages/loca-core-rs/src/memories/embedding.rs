//! Embeddings: L2-normalised vectors for tool relevance and memory relevance,
//! from whichever source the settings select.
//!
//! Two sources, and there is no third:
//!
//! - **Local**, the default. An in-process engine over the MiniLM GGUF, behind
//!   [`LocalEmbedder`]. Present only on a core that can run local inference and
//!   only once the model is on disk.
//! - **External**, when `llm.provider` is `external` **and** a Relevance Model
//!   (`llm.external.embedModel`) is set. Embeddings then reuse that provider's
//!   base URL and API key over an OpenAI-shaped `/embeddings`. There is no
//!   separate provider block, and a blank model means off.
//!
//! **Embeddings are optional.** With no source configured, [`Embedding::embed`]
//! fails with `server_unavailable` and every caller degrades: tools are sent
//! unfiltered, memory injection is skipped, the reindex routes report zero. That
//! is also the state a fresh desktop install is in before the download prompt is
//! accepted, so it is a normal path rather than an error path.
//!
//! # The identity is load-bearing
//!
//! The active model's identity is folded into every stored
//! `embedding_source_hash` ([`relevance::embed_source_hash`]), so a switch
//! between models reads old vectors as stale and re-embeds them instead of
//! comparing incompatible ones. The consequence runs the other way too: a core
//! that spells the **same** model differently reads an existing install's entire
//! vector store as stale, and silently stops injecting memories until a full
//! reindex completes. Nothing errors, and the only symptom is that memories stop
//! being relevant. [`EMBED_REPO`] is that string, and it is pinned by a test
//! against the value the TypeScript emits.
//!
//! [`relevance::embed_source_hash`]: super::relevance::embed_source_hash

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

/// The local embedding model's identity: the MiniLM GGUF the embed engine
/// hosts, named by its repository exactly as `@loca/shared` names it.
///
/// **Do not reformat this string.** It is hashed into every stored
/// `embedding_source_hash`; any other spelling invalidates a real install's
/// whole vector store. It lives here until D6 gives the model catalog a home.
pub const EMBED_REPO: &str = "@second-state/All-MiniLM-L6-v2-Embedding-GGUF/main";

/// Upper bound on one embeddings call. Embedding a handful of short strings is
/// fast; this only stops a wedged-but-listening endpoint from pinning the caller
/// forever.
const EMBED_TIMEOUT_MS: u64 = 30_000;

/// A local, in-process embedding engine.
///
/// Implemented by N2 over a second llama.cpp context. Until then every core runs
/// [`NoLocalEmbedder`], which is exactly the state of a desktop core whose embed
/// model is not downloaded yet.
#[async_trait]
pub trait LocalEmbedder: Send + Sync {
    /// The identity of the model this engine embeds with.
    ///
    /// **The default is correct for the shipped model and should be left
    /// alone.** Override it only if this engine hosts a genuinely different
    /// model, in which case every stored vector *should* read as stale. Getting
    /// this wrong is silent: see the module doc.
    fn model_id(&self) -> &str {
        EMBED_REPO
    }

    /// Whether this engine could embed right now: the model is present and
    /// loadable. Called on the "is it configured" path, so it must be cheap and
    /// must not load anything.
    async fn is_available(&self) -> bool;

    /// One vector per input, in input order, at whatever width the model emits.
    ///
    /// Normalisation is not this trait's job: the caller normalises, so an
    /// engine returning unnormalised vectors is still correct.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Whether this is a real engine rather than [`NoLocalEmbedder`].
    ///
    /// Exists for ONE reason: so a boot-seam test can ask whether the core it
    /// just booted got an embedder, and get an answer that is wrong when the
    /// wiring is missing. [`is_available`](Self::is_available) cannot serve
    /// that, and the difference is the D-159 trap: it answers false for a real
    /// engine whose model is not downloaded, which is the normal state of a
    /// fresh install and of every test's temp home. A seam probe reading it
    /// would be green with the wiring removed and green with it restored.
    ///
    /// Defaults true, so a real engine says so without writing anything.
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

/// The embedding service. Cloning is cheap and every clone shares one identity
/// cache and one HTTP client.
#[derive(Clone)]
pub struct Embedding {
    inner: Arc<Inner>,
}

struct Inner {
    settings: Settings,
    vault: Vault,
    local: Arc<dyn LocalEmbedder>,
    client: reqwest::Client,
    /// The active model identity, refreshed at boot and on every settings
    /// change, so [`Embedding::source_hash`] stays synchronous on the hot path.
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

    /// With no local engine. What a build without `local-inference` runs, and
    /// what a test uses when only the external path is under test.
    pub fn unwired(settings: Settings, vault: Vault) -> Self {
        Self::new(settings, vault, Arc::new(NoLocalEmbedder))
    }

    /// Whether this service got a real local engine.
    ///
    /// The read port for the boot seam. Reads through to the engine boot
    /// actually installed, not to a field boot filled, which is the distinction
    /// the vacuous worker-pool probe got wrong: the property is whether the
    /// SERVICE received it, and only the service can answer that.
    pub fn has_local_engine(&self) -> bool {
        self.inner.local.is_local_engine()
    }

    /// Resolve the active identity now, and keep it current as settings change.
    /// Called once by `boot`.
    pub async fn start(&self) -> Result<()> {
        let settings = self.inner.settings.load_resolved().await?;
        self.refresh_identity(&settings);
        let this = self.clone();
        let mut changes = self.inner.settings.subscribe();
        tokio::spawn(async move {
            loop {
                match changes.recv().await {
                    Ok(change) => this.refresh_identity(&change.merged),
                    // Lagged: the identity is derived from the latest state, so
                    // re-reading it is the whole recovery.
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
    ///
    /// Gates the reindex routes and memory injection, so a caller skips cleanly
    /// rather than hitting an opaque error.
    pub async fn is_ready(&self) -> bool {
        match self.inner.settings.load_resolved().await {
            Ok(settings) => matches!(self.resolve(&settings).await, Ok(Some(_))),
            Err(_) => false,
        }
    }

    /// L2-normalised vectors, one per input, in input order. Empty input returns
    /// empty output without touching any endpoint.
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

    /// Embed one text and pair the vector with its staleness hash, so an index
    /// row can later detect drift by comparing hashes. `None` when the source
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
        // The endpoint returns one entry per input carrying an `index`; sort
        // defensively rather than trusting the order.
        let mut data = body.data.unwrap_or_default();
        data.sort_by_key(|d| d.index.unwrap_or(0));
        Ok(data
            .into_iter()
            .map(|d| {
                d.embedding
                    .unwrap_or_default()
                    .into_iter()
                    // JSON numbers are f64 and the vector is f32: one rounding,
                    // here, before normalisation rounds again on store.
                    .map(|v| v as f32)
                    .collect()
            })
            .collect())
    }

    /// Where to embed, or `None` when embeddings are not configured or not
    /// available.
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

/// The active Relevance Model, or `None` when embeddings are not in external
/// mode. A blank model means off.
fn external_embed_model(settings: &SettingsRecord) -> Option<String> {
    if str_setting(settings, "llm.provider", "local") != "external" {
        return None;
    }
    let model = str_setting(settings, "llm.external.embedModel", "")
        .trim()
        .to_string();
    (!model.is_empty()).then_some(model)
}

/// Check a batch and normalise it.
///
/// The width is the model's own, so only internal consistency is required: a
/// batch whose vectors disagree, or which is missing entries, is a malformed
/// response rather than a usable answer. Cross-model comparison is prevented
/// upstream by the staleness hash, not here.
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

/// L2-normalise in place.
///
/// The local engine already pools to unit vectors and most external providers
/// return them, but normalising unconditionally is what keeps
/// [`super::relevance::cosine_normalized`] (a bare dot product) correct whatever
/// the source does.
///
/// **The accumulation and the division are `f64`; the single rounding to `f32`
/// happens on store.** Squaring and summing in `f32`, or rounding the divisor
/// first, gives a different vector: still unit length, still plausible, and it
/// changes rankings. `testdata/relevance_vectors.json` pins the exact bits, and
/// its emitter refuses to write a fixture that both versions would satisfy.
pub fn l2_normalize(v: &mut [f32]) {
    let mut sum = 0.0f64;
    for x in v.iter() {
        sum += f64::from(*x) * f64::from(*x);
    }
    let norm = sum.sqrt();
    // `> 0.0` rather than `!= 0.0`: a zero vector is left alone, and a NaN norm
    // fails both comparisons identically.
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x = (f64::from(*x) / norm) as f32;
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
