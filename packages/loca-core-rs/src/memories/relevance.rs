//! Embedding-similarity primitives, shared by memory relevance and tool
//! relevance.
//!
//! One implementation so the two stay rank-identical: cosine over L2-normalised
//! vectors, top-K selection, and the embed-text plus staleness-hash pairing the
//! reindex paths persist. Memory injection ([`super::injection`]) is one caller;
//! the tool filter and the extension reindex (D4, D12) are the others, and they
//! call straight into this module rather than growing a second copy.
//!
//! # The arithmetic is the contract
//!
//! Every function here is defined by the numbers it produces, not by its shape,
//! and the two are easy to separate by accident. JavaScript has one numeric
//! type, so `sum += a[i] * b[i]` over two `Float32Array`s widens each lane to
//! `f64`, multiplies in `f64`, and accumulates in `f64`. The natural Rust port
//! accumulates in `f32` and returns a *different number* for the same input:
//! close, plausible, and enough to reorder two memories whose scores are near
//! each other, which is the only time the ordering matters.
//!
//! So: **accumulate in `f64`, return `f64`, and never compare a score
//! approximately in a test.** `testdata/relevance_vectors.json` carries the bit
//! patterns the frozen TypeScript produces, and the emitter refuses to write it
//! unless several cases actually distinguish the two implementations.

use sha2::{Digest, Sha256};

use crate::error::{internal, Result};

/// One scored candidate. `score` is `f64` because a cosine is genuinely
/// fractional (D-030 names it as one of the fields that must not become an
/// integer), and because narrowing it here would discard the precision the
/// accumulation exists to keep.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored<T> {
    pub item: T,
    pub score: f64,
}

/// Cosine similarity of two vectors that are already L2-normalised, which
/// reduces to their dot product.
///
/// Both inputs are assumed normalised, because every producer normalises:
/// [`super::embedding::l2_normalize`] runs over anything an endpoint returns.
///
/// A length mismatch is an error rather than a truncation. Two vectors of
/// different widths came from different embedding models, so scoring the
/// overlap would return a plausible, meaningless number. **Callers pre-filter by
/// dimension** rather than relying on this to skip quietly: one stale vector
/// otherwise aborts a whole batch, which is precisely the memory-injection bug
/// the pre-filter in [`super::injection`] exists to prevent.
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

/// Score every item against the query vector and return the top `top_k` by
/// descending cosine.
///
/// The sort is **stable**, so items with equal scores keep the order they were
/// supplied in. That is observable: three memories embedded from identical text
/// tie exactly, and which one the model sees first is decided here. The
/// insertion order itself is the caller's row order, which for memory injection
/// is SQLite's order for an unordered `SELECT`; adding an `ORDER BY` to make it
/// "deterministic" would change which of two tied memories wins relative to the
/// TypeScript, so it is deliberately left alone.
///
/// `NaN` sorts as equal rather than panicking, matching a JavaScript comparator
/// that returns `NaN` and is treated as zero.
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

/// The staleness hash for an embedded text: `sha256(model_id + "\n" + text)`,
/// lowercase hex.
///
/// The model identity is folded in so that vectors from a different model, which
/// are never comparable, read as stale and re-embed instead of silently
/// mis-scoring. Every freshness comparison against a stored `source_hash` must
/// go through this rather than a bare sha256 of the text.
///
/// `model_id` is a parameter where the TypeScript reads module state, because
/// there are no module globals here (D-010). [`super::embedding::Embedding`]
/// holds the active identity and offers the same call with it applied.
pub fn embed_source_hash(model_id: &str, text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// The text embedded for a memory's relevance vector.
///
/// Defined once so the background indexer and the prompt-time freshness check
/// derive the same string and therefore the same source hash. The indexer passes
/// the summary, or a head slice of the content until a summary exists; the
/// prompt-time skip passes the summary it already has.
pub fn memory_embed_text(title: &str, summary_or_content_head: &str) -> String {
    format!("{title}\n{summary_or_content_head}")
}

/// The text embedded for a tool's relevance vector.
///
/// Defined once so the reindex path and the freshness check derive the same
/// string. The humanised tool name leads, so a query naming the tool ranks it up,
/// followed by the description and the example triggers.
pub fn tool_embed_text<S: AsRef<str>>(name: &str, description: &str, triggers: &[S]) -> String {
    let joined = triggers
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}\n{description}\n{joined}", humanize_name(name))
}

/// True when the user's message names a tool directly: its name, spelled
/// literally (`web_search`) or humanised (`web search`), appears as a run of
/// whole words in the query.
///
/// The match is contiguous and word-bounded, so "open the app store" does not
/// match `open_app` while "run web search" matches `web_search`. A caller
/// force-includes a directly named tool regardless of its embedding rank.
pub fn tool_name_mentioned(query: &str, name: &str) -> bool {
    let needle = normalize_words(name);
    // A one- or two-character name would otherwise match noise.
    if needle.chars().filter(|c| *c != ' ').count() < 3 {
        return false;
    }
    format!(" {} ", normalize_words(query)).contains(&format!(" {needle} "))
}

/// `web_search` -> `web search`, for embedding and matching.
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
    // A trailing separator run produces no trailing space, matching a replace
    // to " " followed by a trim.
    out.trim().to_string()
}

/// Lowercase, collapse every run of non-`[a-z0-9]` to a single space, and trim,
/// so a phrase can be tested as a contiguous whole-word substring.
///
/// The character class is ASCII on purpose: it is `[^a-z0-9]` in the TypeScript,
/// applied after a Unicode lowercase, so an accented letter is a separator in
/// both. Reaching for `char::is_alphanumeric` here would keep it instead and
/// change which queries match.
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
