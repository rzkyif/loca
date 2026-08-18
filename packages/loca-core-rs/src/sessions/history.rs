//! Transcript scans shared by the chat orchestrator and the tool-dispatch path:
//! the newest user message's text (a turn's query, or a tool's chat context) and
//! its id (the turn anchor).
//!
//! Both scan from the tail, so assistant and tool messages inserted during a
//! turn do not change the result.

use super::doc::StoredMessage;
use super::summary::content_to_text;

/// The newest user message's text, or `None` when the transcript has no user
/// message.
pub fn last_user_text(history: &[StoredMessage]) -> Option<String> {
    last_user(history).map(|m| content_to_text(m.content()))
}

/// The newest user message's id, which is what a regenerated turn anchors on.
pub fn last_user_id(history: &[StoredMessage]) -> Option<&str> {
    last_user(history).and_then(StoredMessage::id)
}

fn last_user(history: &[StoredMessage]) -> Option<&StoredMessage> {
    history.iter().rev().find(|m| m.role() == Some("user"))
}
