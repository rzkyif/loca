//! Embedding-similarity primitives shared by memory relevance and tool
//! relevance: cosine over L2-normalised vectors, top-K, the staleness hash,
//! and the embed-text builders.
//!
//! The arithmetic is the contract: accumulate in `f64`, return `f64`. An `f32`
//! accumulator returns a different number and reorders near-tied memories.
//! `testdata/relevance_vectors.json` pins the exact bits and its emitter
//! refuses to write a fixture that both implementations would satisfy.

use sha2::{Digest, Sha256};

use crate::error::{internal, Result};

/// One scored candidate. `score` stays `f64` to keep the accumulated precision.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored<T> {
    pub item: T,
    pub score: f64,
}

/// Cosine of two already-normalised vectors, i.e. their dot product.
///
/// A length mismatch is an error, not a truncation: different widths mean
/// different models. Callers pre-filter by dimension so one stale vector does
/// not abort a whole batch.
pub fn cosine_normalized(a: &[f32], b: &[f32]) -> Result<f64> {
    if a.len() != b.len() {
        return Err(internal(format!(
            "cosine: vector length mismatch ({} vs {}); reindex needed",
            a.len(),
            b.len()
        )));
    }
    let mut sum = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        sum += f64::from(*x) * f64::from(*y);
    }
    Ok(sum)
}

/// Top `top_k` items by descending cosine. The sort is stable, so equal scores
/// keep the caller's order (SQLite row order for memory injection; do not add
/// an `ORDER BY` upstream). `NaN` sorts as equal.
pub fn top_k_by_similarity<T, V: AsRef<[f32]>>(
    query: &[f32],
    items: impl IntoIterator<Item = (T, V)>,
    top_k: usize,
) -> Result<Vec<Scored<T>>> {
    let mut scored = Vec::new();
    for (item, vector) in items {
        scored.push(Scored {
            item,
            score: cosine_normalized(query, vector.as_ref())?,
        });
    }
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(top_k);
    Ok(scored)
}

/// Staleness hash: `sha256(model_id + "\n" + text)`, lowercase hex. The model
/// identity is folded in so vectors from another model read as stale.
pub fn embed_source_hash(model_id: &str, text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// The text a memory's relevance vector is embedded from; the indexer and the
/// prompt-time freshness check must derive the same string.
pub fn memory_embed_text(title: &str, summary_or_content_head: &str) -> String {
    format!("{title}\n{summary_or_content_head}")
}

/// The text a tool's relevance vector is embedded from: humanised name,
/// description, example triggers.
pub fn tool_embed_text<S: AsRef<str>>(name: &str, description: &str, triggers: &[S]) -> String {
    let joined = triggers
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}\n{description}\n{joined}", humanize_name(name))
}

/// True when the tool's name (`web_search` or `web search`) appears as a
/// contiguous, word-bounded run in the query.
pub fn tool_name_mentioned(query: &str, name: &str) -> bool {
    let needle = normalize_words(name);
    // A one- or two-character name would otherwise match noise.
    if needle.chars().filter(|c| *c != ' ').count() < 3 {
        return false;
    }
    format!(" {} ", normalize_words(query)).contains(&format!(" {needle} "))
}

/// `web_search` -> `web search`.
fn humanize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_sep = false;
    for c in name.chars() {
        if c == '_' || c == '-' {
            prev_sep = true;
        } else {
            if prev_sep {
                out.push(' ');
                prev_sep = false;
            }
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Lowercase, collapse runs outside ASCII `[a-z0-9]` to one space, trim. ASCII
/// on purpose: an accented letter is a separator.
fn normalize_words(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        } else {
            pending_space = true;
        }
    }
    out
}
