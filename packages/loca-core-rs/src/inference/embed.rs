//! The in-process embedding engine: a second llama.cpp context over the MiniLM
//! GGUF. Separate from the chat engine because the model is fixed (its repo is
//! hashed into every stored `embedding_source_hash`), it is not gated on
//! `llm.provider` (embeddings are always-on infrastructure), and it needs an
//! `embeddings = true` context with pooling. Loaded lazily on first use and
//! held: at 35 MB, eviction would cost more than it saves.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::error::{internal, AppError, ErrorCode, Result};
use crate::memories::{LocalEmbedder, EMBED_REPO};
use crate::models::manager::resolve_hf_path;
use crate::paths::Paths;

use super::shim;

// Same constant the requirements flow downloads against; two spellings would
// mean waiting forever on a path nothing fetches.
use crate::models::required::EMBED_MODEL_FILE;

/// MiniLM's trained context length; longer inputs are truncated, not refused.
const EMBED_CONTEXT: u32 = 512;

/// Named explicitly rather than left to the model's default: pooling decides
/// the values of every vector, and a mismatch would go unnoticed.
const EMBED_POOLING: &str = "mean";

/// A local embedding engine over one resident MiniLM context.
pub struct LlamaEmbedder {
    paths: Paths,
    /// One mutex over both: `shim::Context` is `Send` and not `Sync`.
    resident: Arc<Mutex<Option<Resident>>>,
}

struct Resident {
    context: shim::Context,
    /// So the error paths can name the width without the context.
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

    /// Blocking work (file read plus graph setup): runs on the blocking pool.
    fn load(path: PathBuf) -> Result<Resident> {
        shim::backend_init();

        let model = Arc::new(shim::Model::load(
            &path.to_string_lossy(),
            &shim::ModelOptions {
                // CPU: too small for offload to pay, and VRAM is for the chat model.
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
        // Cheap and loads nothing; called on every relevance decision.
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
        let handle = tokio::task::spawn_blocking(move || -> Result<Vec<Vec<f32>>> {
            let mut slot = resident.blocking_lock();
            if slot.is_none() {
                if !path.is_file() {
                    // The ordinary state of an install whose model is not downloaded.
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
                // BERT-family pooling is defined over [CLS]/[SEP]; keep them.
                let mut tokens = model.tokenize(text, true, false)?;
                // Truncated, not refused: a long memory should still be indexed.
                tokens.truncate(EMBED_CONTEXT as usize);
                if tokens.is_empty() {
                    // A zero vector keeps the one-vector-per-input contract.
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
