//! The session transcript, as the provider takes it.
//!
//! Three things here are load-bearing:
//!
//! 1. **A multipart user message collapses to a plain string when every part is
//!    text.** Some providers reject a parts array with no `image_url` sibling.
//! 2. **An assistant's `tool_calls` are replayed.** The `tool` messages that
//!    follow reference them by id, and an undeclared `tool_call_id` renders as
//!    an orphaned response in the chat template, garbling every hop after a
//!    call.
//! 3. **`@token` memory references are expanded at request-build time**, not at
//!    store time. The stored message keeps only the token, so editing a memory
//!    flows into the next turn with no rewrite of history.
//!
//! Reasoning, tool-filter, memory-filter, display and error messages are
//! dropped: none of them is part of the message protocol.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::contract::api::{MessageContent, MessagePart};
use crate::error::Result;
use crate::llm::types::{ToolCallFunction, ToolKind};
use crate::llm::{ChatMessage, ContentPart, ImageUrl, ToolCallRef, UserContent};
use crate::memories::injection::{memory_token_blocks, MemoryTokenBudget};
use crate::memories::MemoriesStore;
use crate::sessions::attachments::{
    read_attachment_as_data_url, read_attachment_as_text, AttachmentCache,
};
use crate::sessions::summary::content_to_text;
use crate::sessions::{SessionsStore, StoredMessage};

/// The stores a transcript build reads from, as one value.
pub struct TranscriptSources<'a> {
    pub sessions: &'a SessionsStore,
    pub memories: &'a MemoriesStore,
    pub session_id: &'a str,
}

/// Build the provider's message array from stored history.
///
/// `mcp_claimed` names the slugs an MCP `@resource` or `/prompt` pass already
/// resolved. They are pre-marked as expanded so a slug naming both an MCP
/// resource and a memory is injected once, MCP winning.
///
/// `cache` is per-turn, so a multi-hop conversation reads and base64-encodes
/// each attachment once rather than once per hop.
pub async fn to_chat_messages(
    sources: &TranscriptSources<'_>,
    history: &[StoredMessage],
    system_prompt: &str,
    mcp_claimed: &HashSet<String>,
    cache: &mut AttachmentCache,
) -> Result<Vec<ChatMessage>> {
    let mut out = Vec::new();
    if !system_prompt.is_empty() {
        out.push(ChatMessage::system(system_prompt));
    }
    let mut budget = MemoryTokenBudget::new();
    for stem in mcp_claimed {
        budget.claimed.insert(stem.clone());
    }

    for message in history {
        match message.role().unwrap_or_default() {
            "user" => {
                let mut content = user_content(sources, message.content(), cache).await;
                // The budget dedupes repeat mentions across the whole request,
                // so a memory quoted in ten turns is injected once.
                let blocks = memory_token_blocks(
                    sources.memories,
                    &content_to_text(message.content()),
                    &mut budget,
                )
                .await?;
                if let Some(blocks) = blocks {
                    append_block(&mut content, blocks);
                }
                out.push(ChatMessage::User { content });
            }
            "assistant" => out.push(ChatMessage::Assistant {
                content: content_to_text(message.content()),
                tool_calls: tool_call_refs(message),
            }),
            "system" => out.push(ChatMessage::system(content_to_text(message.content()))),
            "tool" => {
                if let Some(tool) = tool_message(message) {
                    out.push(tool);
                }
            }
            // `reasoning` is not part of the message protocol; `tool_filter`,
            // `memory_filter`, `display` and `error` are client-facing bubbles
            // the model never sees.
            _ => {}
        }
    }
    Ok(out)
}

/// One user message's content, with attachments resolved.
async fn user_content(
    sources: &TranscriptSources<'_>,
    content: Option<&Value>,
    cache: &mut AttachmentCache,
) -> UserContent {
    let Some(value) = content else {
        return UserContent::Text(String::new());
    };
    // A part shape the contract does not know fails the whole deserialize, so
    // the message degrades to its flattened text rather than being dropped.
    let parsed: MessageContent = match serde_json::from_value(value.clone()) {
        Ok(parsed) => parsed,
        Err(_) => return UserContent::Text(content_to_text(content)),
    };
    let parts = match parsed {
        MessageContent::Variant0(text) => return UserContent::Text(text),
        MessageContent::Variant1(parts) => parts,
    };

    let mut out: Vec<ContentPart> = Vec::new();
    for part in parts {
        match part {
            MessagePart::Text { text } => out.push(ContentPart::Text { text }),
            MessagePart::ImageUrl { image_url } => out.push(ContentPart::ImageUrl {
                image_url: ImageUrl { url: image_url.url },
            }),
            MessagePart::ImageFile { mime, path, .. } => {
                let fallback = if mime.is_empty() { "image/png" } else { &mime };
                // A `None` is an unreadable attachment, already logged by the
                // reader. The part is skipped and the turn still runs.
                if let Some(url) = read_attachment_as_data_url(
                    sources.sessions,
                    sources.session_id,
                    &path,
                    fallback,
                    Some(cache),
                )
                .await
                {
                    out.push(ContentPart::ImageUrl {
                        image_url: ImageUrl { url },
                    });
                }
            }
            MessagePart::Document { filename, markdown } => out.push(ContentPart::Text {
                text: document_block(&filename, &markdown),
            }),
            MessagePart::DocumentFile { filename, path } => {
                if let Some(text) = read_attachment_as_text(
                    sources.sessions,
                    sources.session_id,
                    &path,
                    Some(cache),
                )
                .await
                {
                    out.push(ContentPart::Text {
                        text: document_block(&filename, &text),
                    });
                }
            }
        }
    }

    // Collapse to a plain string when nothing needs the multipart form. Some
    // providers reject a parts array carrying only text.
    if out.iter().all(|p| matches!(p, ContentPart::Text { .. })) {
        let joined: Vec<&str> = out
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ImageUrl { .. } => None,
            })
            .collect();
        return UserContent::Text(joined.join("\n"));
    }
    UserContent::Parts(out)
}

/// The header exists so the model can see where the attached document starts
/// and ends.
fn document_block(filename: &str, body: &str) -> String {
    format!("[Attached document: {filename}]\n\n{body}")
}

fn append_block(content: &mut UserContent, blocks: String) {
    match content {
        UserContent::Text(text) => {
            if text.is_empty() {
                *text = blocks;
            } else {
                text.push_str("\n\n");
                text.push_str(&blocks);
            }
        }
        UserContent::Parts(parts) => parts.push(ContentPart::Text { text: blocks }),
    }
}

/// The calls an assistant message emitted, in the shape the API replays them.
fn tool_call_refs(message: &StoredMessage) -> Vec<ToolCallRef> {
    let Some(Value::Array(calls)) = message.as_map().get("toolCalls") else {
        return Vec::new();
    };
    calls
        .iter()
        .filter_map(|call| {
            let call = call.as_object()?;
            Some(ToolCallRef {
                id: str_field(call, "callId")?.to_string(),
                kind: ToolKind::Function,
                function: ToolCallFunction {
                    name: str_field(call, "toolName")?.to_string(),
                    arguments: str_field(call, "arguments").unwrap_or("{}").to_string(),
                },
            })
        })
        .collect()
}

/// A tool result, as the model reads it: the raw result on success, and
/// `{"error": ...}` on anything else.
fn tool_message(message: &StoredMessage) -> Option<ChatMessage> {
    let map = message.as_map();
    let tool_call_id = str_field(map, "callId")?.to_string();
    let content = if str_field(map, "status") == Some("completed") {
        serde_json::to_string(map.get("result").unwrap_or(&Value::Null)).ok()?
    } else {
        // Built rather than `json!` so an absent error serialises as `{}`: a
        // `{"error":null}` would tell the model the tool reported a null error.
        let mut wrapper = Map::new();
        if let Some(error) = map.get("error").filter(|v| !v.is_null()) {
            wrapper.insert("error".into(), error.clone());
        }
        serde_json::to_string(&Value::Object(wrapper)).ok()?
    };
    Some(ChatMessage::Tool {
        tool_call_id,
        content,
    })
}

fn str_field<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}
