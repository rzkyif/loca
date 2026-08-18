//! Background indexing: keeping each memory's summary and embedding in step
//! with its content.
//!
//! One pass per memory. The pass is cheap when both are fresh, because freshness
//! is a hash comparison, so callers schedule liberally: at boot, and after every
//! mutation. Nothing is persisted about the work itself; staleness is re-derived
//! from the hashes, so a job dropped at shutdown is found again on the next boot.
//!
//! # The two seams
//!
//! [`MemoryIndexer`] is **when** a pass runs. The desktop core drains passes
//! through an idle-gated background queue that waits for chat to go quiet, so
//! background work never competes with a user-visible stream for the model. That
//! queue is D4's, because its idle signal is chat's; this module only asks for a
//! pass and does not care who schedules it.
//!
//! [`SummaryGenerator`] is **who talks to the model**. The single-shot LLM call
//! is D3's, so this module builds the memory-specific half (which prompt, which
//! budget key, how much content) and hands it over. With [`NoSummaries`]
//! installed the summary step is skipped and the embedding step still runs,
//! which is the correct degraded behaviour rather than a stub: a core with no
//! LLM configured should still embed.

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

/// The settings key holding this job's reasoning budget. Named here rather than
/// inline because the request carries it to whoever resolves budgets.
const SUMMARY_BUDGET_KEY: &str = "prompts.memorySummaryThinkingBudget";

/// Token allowance for the summary itself, before any thinking budget is added
/// on top.
const SUMMARY_MAX_TOKENS: u32 = 160;

/// Schedules index passes. Implemented by whoever owns the background queue;
/// the routes and the store only ever ask.
pub trait MemoryIndexer: Send + Sync {
    /// Queue a pass for one memory, or for every memory when no id is given
    /// (boot, rescan).
    fn schedule(&self, memory_id: Option<&str>);
}

/// Drops every request. What a core runs before a queue is wired, and what a
/// route-level test installs so scheduling has no side effects.
pub struct NoIndexer;

impl MemoryIndexer for NoIndexer {
    fn schedule(&self, _memory_id: Option<&str>) {}
}

/// One non-streaming completion, for a background summary.
///
/// The implementation lives with D3's single-shot path and applies
/// `thinking_budget_key` and the endpoint resolution; everything memory-specific
/// is decided here and arrives in the request.
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
    /// Deterministic: a summary is an index entry, not a creative answer.
    pub temperature: f64,
    /// The answer's own allowance. The implementation adds the resolved thinking
    /// budget to it, so the answer still has room after any reasoning.
    pub max_tokens: u32,
    /// Settings key holding this job's reasoning budget. Zero turns thinking
    /// off, which is this job's default.
    pub thinking_budget_key: &'static str,
}

/// No model configured: every summary request is skipped, and the pass proceeds
/// to the embedding step.
pub struct NoSummaries;

#[async_trait]
impl SummaryGenerator for NoSummaries {
    async fn generate(&self, _request: SummaryRequest) -> Result<Option<String>> {
        Ok(None)
    }
}

/// What one pass did, so a caller (and a test) can tell a no-op from work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexOutcome {
    pub summarized: bool,
    pub embedded: bool,
    /// The embedding step could not run because no embedding source is
    /// available. The caller re-arms the pass rather than dropping it, so the
    /// vector lands once the model is downloaded instead of waiting for the next
    /// unrelated mutation.
    pub embedding_deferred: bool,
}

/// Bring one memory's summary and embedding up to date.
///
/// Order matters: the embedding is derived from the title plus the summary, so
/// the summary is refreshed first and the embedding picks it up in the same
/// pass. A memory deleted while the pass was queued is a no-op, not an error.
pub async fn index_memory(
    store: &MemoriesStore,
    embedding: &Embedding,
    summaries: &Arc<dyn SummaryGenerator>,
    settings: &Settings,
    id: &str,
) -> Result<IndexOutcome> {
    let mut outcome = IndexOutcome::default();
    let Some(state) = find_state(store, id).await? else {
        return Ok(outcome); // deleted while queued
    };

    // Summary: regenerate when the stored one was derived from other content.
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
            // An empty summary is no summary: storing it would pin
            // `summary_source_hash` to the current content, so the row reads
            // fresh forever and the memory is never summarised. Left alone, the
            // next pass retries.
            Ok(Some(summary)) if summary.is_empty() => {}
            Ok(Some(summary)) => {
                store.set_summary(id, &summary, &state.content_hash).await?;
                outcome.summarized = true;
            }
            Ok(None) => {}
            // Never fatal: the work is re-found on the next mutation or boot.
            Err(err) => tracing::warn!(
                target: "loca::memories",
                "summary for memory {id} failed: {}", err.message,
            ),
        }
    }

    // Embedding: derived from title plus summary, or the content head until a
    // summary exists, so it refreshes after the summary above lands.
    if !embedding.is_ready().await {
        // Guard the re-arm with the hash, so a steady not-ready state does not
        // spin: only ask again when there is real work waiting.
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
        return Ok(outcome); // fresh
    }
    if let Some(result) = embedding.embed_with_hash(&text).await? {
        store
            .set_embedding(id, &result.vector, &result.source_hash)
            .await?;
        outcome.embedded = true;
    }
    Ok(outcome)
}

/// Every memory's id, for a full pass at boot or after a rescan.
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

/// The text a memory is embedded from: its title plus its summary, or a head
/// slice of its content until a summary exists.
fn embed_text(doc: &Memory) -> String {
    let source = match doc.summary.as_deref() {
        Some(summary) => summary.to_string(),
        None => head(&doc.content, EMBED_FALLBACK_HEAD_CHARS),
    };
    memory_embed_text(&doc.title, &source)
}

/// The configured summary prompt, or the schema default when it is blank.
///
/// The fallback is not decoration: a user who clears the field gets the default
/// back rather than an empty system prompt, and `load_resolved` cannot express
/// that because an explicit empty string is a legitimate stored value.
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

/// The first `cap` UTF-16 code units, cut on a character boundary. Same counting
/// as the injection budget, for the same reason.
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
