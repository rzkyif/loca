//! Conversation snippets for the session list, and the content-to-text
//! flattening they and the chat history scan share.

use serde_json::Value;

use crate::contract::api::{SummaryPart, SummaryPartRole};

use super::doc::StoredMessage;

const SUMMARY_MAX_CHARS: usize = 160;
const SUMMARY_SNIPPET_MAX_CHARS: usize = 60;

/// The text of a `MessageContent`: the value itself when it is a string, else
/// the `text` parts of a multipart array, concatenated. Every other part kind
/// (images, documents, files) contributes nothing.
pub fn content_to_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

/// The opening user and assistant exchange, as many turns as fit the budget.
///
/// Labels are left to the client, which knows the configured agent name. Empty
/// when the session has no user or assistant message yet.
pub fn summarize_conversation(messages: &[StoredMessage]) -> Vec<SummaryPart> {
    let mut parts: Vec<SummaryPart> = Vec::new();
    let mut total = 0usize;
    for message in messages {
        let role = match message.role() {
            Some("user") => SummaryPartRole::User,
            Some("assistant") => SummaryPartRole::Agent,
            _ => continue,
        };
        let text = collapse_whitespace(&content_to_text(message.content()));
        if text.is_empty() {
            continue;
        }
        // Counted in characters rather than UTF-16 code units. The budget is a
        // display bound on a snippet the client truncates to one line anyway, so
        // the two differ only outside the basic plane and neither is contractual.
        let snippet = if text.chars().count() > SUMMARY_SNIPPET_MAX_CHARS {
            let head: String = text.chars().take(SUMMARY_SNIPPET_MAX_CHARS).collect();
            format!("{head}\u{2026}")
        } else {
            text
        };
        let len = snippet.chars().count();
        // The first snippet is always taken, however long: a session whose
        // opening message alone blows the budget would otherwise summarize to
        // nothing at all.
        if !parts.is_empty() && total + len > SUMMARY_MAX_CHARS {
            break;
        }
        parts.push(SummaryPart {
            role,
            text: snippet,
        });
        total += len;
    }
    parts
}

/// Trim, then collapse every run of whitespace to a single space.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, word) in text.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}
