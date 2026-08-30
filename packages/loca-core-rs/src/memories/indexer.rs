//! Background indexing: keeping each memory's summary and embedding in step
//! with its content. Freshness is a hash comparison, so passes are cheap and
//! scheduled liberally (boot, every mutation); nothing about the work itself
//! is persisted.
//!
//! [`MemoryIndexer`] is WHEN a pass runs (the background queue's business);
//! [`SummaryGenerator`] is WHO talks to the model. With [`NoSummaries`] the
//! summary step is skipped and the embedding step still runs.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::Result;
use crate::settings::access::str_setting;
use crate::settings::{schema, Settings};

use super::embedding::Embedding;
use super::relevance::memory_embed_text;
use super::store::MemoriesStore;
use crate::contract::api::Memory;

/// Caps so one huge pasted memory cannot blow up the summary request or the
/// embedding text.
const SUMMARY_INPUT_MAX_CHARS: usize = 8_000;
const EMBED_FALLBACK_HEAD_CHARS: usize = 512;

/// The settings key holding this job's reasoning budget.
const SUMMARY_BUDGET_KEY: &str = "prompts.memorySummaryThinkingBudget";

/// Token allowance for the summary itself, before any thinking budget.
const SUMMARY_MAX_TOKENS: u32 = 160;

/// Schedules index passes.
pub trait MemoryIndexer: Send + Sync {
    /// Queue a pass for one memory, or for every memory when no id is given.
    fn schedule(&self, memory_id: Option<&str>);
}

/// Drops every request.
pub struct NoIndexer;

impl MemoryIndexer for NoIndexer {
    fn schedule(&self, _memory_id: Option<&str>) {}
}

/// One non-streaming completion, for a background summary.
#[async_trait]
pub trait SummaryGenerator: Send + Sync {
    /// The summary text, or `None` when the model returned nothing usable.
    async fn generate(&self, request: SummaryRequest) -> Result<Option<String>>;
}

/// What a summary pass asks the model for.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryRequest {
    pub system_prompt: String,
    pub user_message: String,
    pub temperature: f64,
    /// The answer's own allowance; the implementation adds the thinking budget.
    pub max_tokens: u32,
    /// Settings key holding this job's reasoning budget.
    pub thinking_budget_key: &'static str,
}

/// No model configured: every summary request is skipped.
pub struct NoSummaries;

#[async_trait]
impl SummaryGenerator for NoSummaries {
    async fn generate(&self, _request: SummaryRequest) -> Result<Option<String>> {
        Ok(None)
    }
}

/// What one pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexOutcome {
    pub summarized: bool,
    pub embedded: bool,
    /// No embedding source was available while work was waiting; the caller
    /// re-arms the pass so the vector lands once the model is downloaded.
    pub embedding_deferred: bool,
}

/// Bring one memory's summary and embedding up to date. The summary is
/// refreshed first because the embedding derives from it. A memory deleted
/// while queued is a no-op.
pub async fn index_memory(
    store: &MemoriesStore,
    embedding: &Embedding,
    summaries: &Arc<dyn SummaryGenerator>,
    settings: &Settings,
    id: &str,
) -> Result<IndexOutcome> {
    let mut outcome = IndexOutcome::default();
    let Some(state) = find_state(store, id).await? else {
        return Ok(outcome);
    };

    if state.summary_source_hash.as_deref() != Some(state.content_hash.as_str()) {
        let doc = store.get(id).await?;
        let settings = settings.load_resolved().await?;
        let request = SummaryRequest {
            system_prompt: summary_prompt(&settings),
            user_message: format!(
                "Memory title: {}\n\n{}",
                doc.title,
                head(&doc.content, SUMMARY_INPUT_MAX_CHARS)
            ),
            temperature: 0.0,
            max_tokens: SUMMARY_MAX_TOKENS,
            thinking_budget_key: SUMMARY_BUDGET_KEY,
        };
        match summaries.generate(request).await {
            // An empty summary is no summary: storing it would pin the hash and
            // the memory would never be summarised.
            Ok(Some(summary)) if summary.is_empty() => {}
            Ok(Some(summary)) => {
                store.set_summary(id, &summary, &state.content_hash).await?;
                outcome.summarized = true;
            }
            Ok(None) => {}
            // Never fatal: re-found on the next mutation or boot.
            Err(err) => tracing::warn!(
                target: "loca::memories",
                "summary for memory {id} failed: {}", err.message,
            ),
        }
    }

    if !embedding.is_ready().await {
        // Only ask to be re-armed when real work is waiting, so a steady
        // not-ready state does not spin.
        if let Some(pending) = find_state(store, id).await? {
            let doc = store.get(id).await?;
            let text = embed_text(&doc);
            if pending.embedding_source_hash != Some(embedding.source_hash(&text)) {
                outcome.embedding_deferred = true;
            }
        }
        return Ok(outcome);
    }
    let doc = store.get(id).await?;
    let text = embed_text(&doc);
    let Some(current) = find_state(store, id).await? else {
        return Ok(outcome);
    };
    if current.embedding_source_hash == Some(embedding.source_hash(&text)) {
        return Ok(outcome);
    }
    if let Some(result) = embedding.embed_with_hash(&text).await? {
        store
            .set_embedding(id, &result.vector, &result.source_hash)
            .await?;
        outcome.embedded = true;
    }
    Ok(outcome)
}

/// Every memory's id, for a full pass.
pub async fn all_memory_ids(store: &MemoriesStore) -> Result<Vec<String>> {
    Ok(store
        .list_index_states()
        .await?
        .into_iter()
        .map(|s| s.id)
        .collect())
}

async fn find_state(store: &MemoriesStore, id: &str) -> Result<Option<super::store::IndexState>> {
    Ok(store
        .list_index_states()
        .await?
        .into_iter()
        .find(|s| s.id == id))
}

/// Title plus summary, or a head slice of the content until a summary exists.
fn embed_text(doc: &Memory) -> String {
    let source = match doc.summary.as_deref() {
        Some(summary) => summary.to_string(),
        None => head(&doc.content, EMBED_FALLBACK_HEAD_CHARS),
    };
    memory_embed_text(&doc.title, &source)
}

/// The configured summary prompt, or the schema default when blank (a cleared
/// field must not become an empty system prompt).
fn summary_prompt(settings: &crate::settings::SettingsRecord) -> String {
    let configured = str_setting(settings, "prompts.memorySummaryPrompt", "");
    if !configured.trim().is_empty() {
        return configured;
    }
    schema()
        .defaults()
        .get("prompts.memorySummaryPrompt")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// The first `cap` UTF-16 code units, cut on a character boundary.
fn head(s: &str, cap: usize) -> String {
    let mut used = 0usize;
    let mut end = 0usize;
    for (offset, c) in s.char_indices() {
        let width = c.len_utf16();
        if used + width > cap {
            return s[..end].to_string();
        }
        used += width;
        end = offset + c.len_utf8();
    }
    s.to_string()
}
