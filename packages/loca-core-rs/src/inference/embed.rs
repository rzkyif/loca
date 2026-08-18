//! The in-process embedding engine: a second llama.cpp context over the MiniLM
//! GGUF, replacing the oracle's second `llama-server`.
//!
//! # What this replaces, and what was lost without it
//!
//! The shipping TypeScript core runs `sidecars/llama-embed.ts`: a whole second
//! `llama-server` process, hosting `all-MiniLM-L6-v2-Q8_0.gguf` and serving
//! OpenAI-shaped `/v1/embeddings`. The Rust port shipped the
//! [`LocalEmbedder`](crate::memories::LocalEmbedder) trait, the model identity
//! and [`NoLocalEmbedder`](crate::memories::NoLocalEmbedder), whose doc said
//! "implemented by N2", and N2 did not implement it. So every core ran with no
//! local embedder at all.
//!
//! That failure is quiet by construction, which is why it survived: embeddings
//! are OPTIONAL by design. With no source configured, `Embedding::embed` fails
//! with `server_unavailable` and every caller degrades on purpose rather than
//! erroring, so memory injection is skipped, tool relevance sends the model an
//! unfiltered catalog, and the reindex routes report zero. Each of those is also
//! the correct behaviour of a fresh install whose model has not been downloaded
//! yet, so nothing anywhere distinguishes "not downloaded" from "never built".
//!
//! # Not a mode of the generation engine
//!
//! A separate context over a separate model, and deliberately not a flag on
//! [`LocalEngine`](super::engine::LocalEngine):
//!
//! - The model is FIXED. [`EMBED_REPO`] is hashed into every stored
//!   `embedding_source_hash`, so unlike the chat model this one cannot follow a
//!   setting without invalidating an install's whole vector store.
//! - It is NOT gated on `llm.provider`. The oracle's header says so in as many
//!   words: embeddings are always-on infrastructure. A user answering from
//!   OpenAI still wants their memories to work, and the chat engine is unloaded
//!   in exactly that case.
//! - The llama.cpp call is different. Generation is `llama_decode` against a KV
//!   cache; embedding is `llama_encode` with pooling and no cache at all, on a
//!   context created with `embeddings = true`. They cannot share a context.
//!
//! # Residency
//!
//! Loaded lazily on first use and then held. The MiniLM Q8 GGUF is 35 MB, which
//! is three orders of magnitude below the chat model, so the eviction machinery
//! the chat engine needs would cost more than it saves. Nothing unloads this
//! short of core shutdown.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::error::{internal, AppError, ErrorCode, Result};
use crate::memories::{LocalEmbedder, EMBED_REPO};
use crate::models::manager::resolve_hf_path;
use crate::paths::Paths;

use super::shim;

// The GGUF this engine hosts. Taken from `models::required` rather than spelled
// again here, because that is the constant the requirements flow downloads
// against: two spellings would mean the downloader fetching one path while this
// waits forever on another, with no error anywhere, just embeddings that never
// start working.
use crate::models::required::EMBED_MODEL_FILE;

/// MiniLM's trained context length.
///
/// The oracle passes `-c 512` (`llama-embed.ts:19`). Inputs here are tool
/// descriptions and memory chunks, all far below it, and llama.cpp caps the
/// embedding batch to `n_ubatch` regardless. A token sequence longer than this
/// is truncated rather than refused, which is what the oracle's server does.
const EMBED_CONTEXT: u32 = 512;

/// How the per-token states collapse into one vector.
///
/// Named explicitly, matching the oracle's `--pooling mean`
/// (`llama-embed.ts:59-60`), rather than left to the model's trained default.
/// The default would very likely also be mean for this GGUF, but "very likely"
/// is not good enough: pooling decides the VALUES of every vector, and a
/// mismatch against the oracle produces embeddings that are wrong in a way no
/// test would notice, because a cosine similarity between two consistently-wrong
/// vectors still looks like a similarity.
const EMBED_POOLING: &str = "mean";

/// A local embedding engine over one resident MiniLM context.
pub struct LlamaEmbedder {
    paths: Paths,
    /// The model and its context, loaded on first use.
    ///
    /// One mutex over both, not two: `shim::Context` is `Send` and not `Sync`
    /// (llama.cpp has no internal locking), so exactly one caller may touch it
    /// at a time and the lock is what enforces that. Embedding a handful of
    /// short strings takes milliseconds, so serialising costs nothing worth
    /// measuring.
    resident: Arc<Mutex<Option<Resident>>>,
}

struct Resident {
    context: shim::Context,
    /// Kept so `is_available` and the error paths can name the width without
    /// reaching back through the context.
    n_embd: usize,
}

impl LlamaEmbedder {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            resident: Arc::new(Mutex::new(None)),
        }
    }

    /// Where the requirements flow puts the GGUF.
    fn model_path(&self) -> PathBuf {
        resolve_hf_path(self.paths.models_dir(), EMBED_MODEL_FILE)
    }

    /// Load the model and build the embedding context.
    ///
    /// Blocking work (a 35 MB read plus graph setup), so it runs on the blocking
    /// pool rather than a runtime worker, for the same reason the chat engine's
    /// load does.
    fn load(path: PathBuf) -> Result<Resident> {
        shim::backend_init();

        let model = Arc::new(shim::Model::load(
            &path.to_string_lossy(),
            &shim::ModelOptions {
                // CPU. The embed model is small enough that a GPU offload would
                // cost more in transfers than it saves, and on a machine with a
                // chat model resident the VRAM is better spent there.
                n_gpu_layers: 0,
                use_mmap: true,
                use_mlock: false,
            },
        )?);

        let n_embd = model.n_embd();
        if n_embd <= 0 {
            return Err(internal(format!(
                "{} is not an embedding model: it reports no embedding width",
                path.display()
            )));
        }

        let context = shim::Context::new(
            model,
            &shim::ContextOptions {
                n_ctx: EMBED_CONTEXT,
                n_batch: EMBED_CONTEXT,
                n_threads: 4,
                flash_attn: false,
                embeddings: true,
                pooling_type: EMBED_POOLING.to_string(),
            },
        )?;

        Ok(Resident {
            context,
            n_embd: n_embd as usize,
        })
    }
}

#[async_trait]
impl LocalEmbedder for LlamaEmbedder {
    fn model_id(&self) -> &str {
        EMBED_REPO
    }

    async fn is_available(&self) -> bool {
        // Cheap and loads nothing, as the trait requires: this is called on the
        // "is it configured" path, which runs on every relevance decision. The
        // file being present is exactly the condition the requirements flow
        // resolves, so it is also the right answer before the first load.
        tokio::fs::metadata(self.model_path())
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let path = self.model_path();
        let resident = Arc::clone(&self.resident);
        let texts = texts.to_vec();

        // The whole batch on one blocking task, holding the lock across it.
        // llama.cpp decode is CPU-bound and would starve the runtime's workers;
        // the guard travels with the closure because the context is `Send`.
        let handle = tokio::task::spawn_blocking(move || -> Result<Vec<Vec<f32>>> {
            let mut slot = resident.blocking_lock();
            if slot.is_none() {
                if !path.is_file() {
                    // Not an internal error: this is the ordinary state of an
                    // install whose model has not been downloaded yet, and the
                    // caller's contract is to degrade rather than fail loudly.
                    return Err(AppError::new(
                        ErrorCode::ServerUnavailable,
                        format!(
                            "the local embedding model is not on disk yet ({}). It downloads \
                             with the other required models once you accept the prompt.",
                            path.display()
                        ),
                    ));
                }
                *slot = Some(LlamaEmbedder::load(path)?);
            }
            let Some(resident) = slot.as_mut() else {
                return Err(internal("the embedding context vanished after loading"));
            };

            let model = resident.context.model();
            let mut out = Vec::with_capacity(texts.len());
            for text in &texts {
                // `add_special` true: MiniLM is a BERT-family model and its
                // pooling is defined over a sequence that carries [CLS]/[SEP].
                // Dropping them changes every vector.
                let mut tokens = model.tokenize(text, true, false)?;
                // Truncated rather than refused, matching the oracle's server.
                // An over-long input is a long memory, not a caller error, and
                // refusing would drop it out of the index entirely.
                tokens.truncate(EMBED_CONTEXT as usize);
                if tokens.is_empty() {
                    // Nothing to encode. A zero vector is the honest answer and
                    // keeps the one-vector-per-input contract, which the caller
                    // zips against its inputs by position.
                    out.push(vec![0.0; resident.n_embd]);
                    continue;
                }
                out.push(resident.context.embed(&tokens)?);
            }
            Ok(out)
        })
        .await;

        match handle {
            Ok(result) => result,
            Err(err) => Err(internal(format!("the embedding task failed: {err}"))),
        }
    }
}
