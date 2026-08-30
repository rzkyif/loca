//! Server-sent events, and the OpenAI chunk shapes they carry.
//!
//! Hand-written rather than taken from a crate: an OpenAI-compatible completion
//! sends one narrow profile of the spec, `data:` lines with a blank line ending
//! each event and a `[DONE]` sentinel, with no `event:` names, no `id:`, no
//! reconnection and no retry backoff. A framing bug here surfaces as a model
//! that skips words, so the decoder is small, owned, and tested against a
//! deliberately fragmented feed.
//!
//! This is the EXTERNAL path's business: the in-process engine emits
//! [`super::LlmDelta`] directly and never sees a byte of SSE.

use serde::Deserialize;

use crate::contract::api::TokenUsage;

use super::types::{LlmDelta, ToolCallDelta};

/// The payload that ends a stream. Not JSON, so it is matched before parsing.
pub const DONE_SENTINEL: &str = "[DONE]";

/// Reassembles events from arbitrarily chunked bytes.
///
/// A chunk boundary lands mid-event routinely (a 4 KB read splitting a JSON
/// payload is the common case, not the edge case), so the decoder buffers until
/// it has a whole event and never parses a partial one.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// The next complete event's data payload, or `None` while one is still
    /// arriving.
    ///
    /// Events carrying no `data:` line at all (a `:` heartbeat comment, which
    /// some gateways send to hold the connection open) are consumed and skipped
    /// rather than surfaced as empty payloads.
    pub fn next_event(&mut self) -> Option<String> {
        loop {
            let (end, skip) = self.find_event_end()?;
            let block = self.buf.drain(..end + skip).collect::<Vec<u8>>();
            let block = String::from_utf8_lossy(&block[..end]).into_owned();
            let mut data = String::new();
            let mut saw_data = false;
            for line in block.split('\n') {
                let line = line.strip_suffix('\r').unwrap_or(line);
                let Some(rest) = line.strip_prefix("data:") else {
                    continue;
                };
                if saw_data {
                    data.push('\n');
                }
                saw_data = true;
                // One optional space after the colon is field separator, per the
                // spec; anything beyond it is payload.
                data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            }
            if saw_data {
                return Some(data);
            }
        }
    }

    /// Offset of the end of the first complete event, and the length of the
    /// blank-line terminator that follows it.
    fn find_event_end(&self) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        for (pattern, skip) in [
            (&b"\r\n\r\n"[..], 4usize),
            (&b"\n\n"[..], 2),
            (&b"\r\r"[..], 2),
        ] {
            if let Some(at) = find(&self.buf, pattern) {
                if best.is_none_or(|(current, _)| at < current) {
                    best = Some((at, skip));
                }
            }
        }
        best
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// One `chat.completion.chunk`, with only the fields the core reads.
///
/// Unknown fields are ignored by default, which is what keeps a provider's extra
/// bookkeeping from failing a stream mid-completion.
#[derive(Debug, Deserialize)]
pub struct ChatChunk {
    #[serde(default)]
    pub choices: Vec<ChunkChoice>,
    #[serde(default)]
    pub usage: Option<ChunkUsage>,
}

#[derive(Debug, Deserialize)]
pub struct ChunkChoice {
    #[serde(default)]
    pub delta: Option<ChunkDelta>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChunkDelta {
    #[serde(default)]
    pub content: Option<String>,
    /// llama.cpp and DeepSeek-style reasoning.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// The flat spelling some OpenAI-compatible gateways use instead.
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ChunkToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct ChunkToolCall {
    pub index: u32,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<ChunkFunction>,
}

#[derive(Debug, Deserialize)]
pub struct ChunkFunction {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChunkUsage {
    #[serde(default)]
    pub prompt_tokens: Option<i64>,
    #[serde(default)]
    pub completion_tokens: Option<i64>,
    #[serde(default)]
    pub total_tokens: Option<i64>,
}

impl ChatChunk {
    /// Normalise into the core's own delta shape.
    ///
    /// Only the first choice is read: `n > 1` is never requested, and a second
    /// choice would interleave two completions into one stream.
    pub fn to_delta(&self) -> LlmDelta {
        let mut delta = LlmDelta::default();
        let choice = self.choices.first();
        if let Some(inner) = choice.and_then(|c| c.delta.as_ref()) {
            // Empty strings are dropped rather than yielded: the opening chunk
            // of most providers carries `content: ""` beside the role, and
            // passing it on would make a consumer counting deltas see a token
            // that was never generated.
            if let Some(content) = non_empty(&inner.content) {
                delta.content_delta = Some(content);
            }
            // `reasoning_content` wins when a provider sends both.
            if let Some(reasoning) =
                non_empty(&inner.reasoning_content).or_else(|| non_empty(&inner.reasoning))
            {
                delta.reasoning_delta = Some(reasoning);
            }
            if let Some(calls) = &inner.tool_calls {
                delta.tool_calls = calls
                    .iter()
                    .map(|call| ToolCallDelta {
                        index: call.index,
                        id: call.id.clone(),
                        name: call.function.as_ref().and_then(|f| f.name.clone()),
                        arguments_delta: call.function.as_ref().and_then(|f| f.arguments.clone()),
                    })
                    .collect();
            }
        }
        if let Some(reason) = choice.and_then(|c| non_empty(&c.finish_reason)) {
            delta.finish_reason = Some(reason);
        }
        if let Some(usage) = &self.usage {
            delta.usage = Some(TokenUsage {
                prompt: usage.prompt_tokens.unwrap_or(0),
                completion: usage.completion_tokens.unwrap_or(0),
                total: usage.total_tokens.unwrap_or(0),
            });
        }
        delta
    }
}

fn non_empty(value: &Option<String>) -> Option<String> {
    value.as_ref().filter(|s| !s.is_empty()).cloned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn events(decoder: &mut SseDecoder) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(event) = decoder.next_event() {
            out.push(event);
        }
        out
    }

    #[test]
    fn events_are_split_on_the_blank_line() {
        let mut decoder = SseDecoder::new();
        decoder.push(b"data: one\n\ndata: two\n\n");
        assert_eq!(events(&mut decoder), vec!["one", "two"]);
    }

    #[test]
    fn a_partial_event_is_held_until_it_completes() {
        let mut decoder = SseDecoder::new();
        decoder.push(b"data: {\"a\":");
        assert_eq!(decoder.next_event(), None);
        decoder.push(b"1}\n\n");
        assert_eq!(decoder.next_event(), Some("{\"a\":1}".to_string()));
    }

    #[test]
    fn crlf_terminators_and_comment_only_events_are_handled() {
        let mut decoder = SseDecoder::new();
        decoder.push(b": keep-alive\r\n\r\ndata: payload\r\n\r\n");
        assert_eq!(events(&mut decoder), vec!["payload"]);
    }

    #[test]
    fn multi_line_data_is_joined_with_newlines() {
        let mut decoder = SseDecoder::new();
        decoder.push(b"data: first\ndata: second\n\n");
        assert_eq!(events(&mut decoder), vec!["first\nsecond"]);
    }

    #[test]
    fn a_feed_split_one_byte_at_a_time_decodes_identically() {
        let feed = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut whole = SseDecoder::new();
        whole.push(feed.as_bytes());
        let mut byte_at_a_time = SseDecoder::new();
        let mut split = Vec::new();
        for byte in feed.as_bytes() {
            byte_at_a_time.push(&[*byte]);
            while let Some(event) = byte_at_a_time.next_event() {
                split.push(event);
            }
        }
        assert_eq!(events(&mut whole), split);
        assert_eq!(split.len(), 3);
        assert_eq!(split[2], DONE_SENTINEL);
    }

    #[test]
    fn the_opening_role_chunk_produces_nothing() {
        let chunk: ChatChunk = serde_json::from_str(
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
        )
        .unwrap();
        assert!(chunk.to_delta().is_empty());
    }

    #[test]
    fn reasoning_content_wins_over_the_flat_spelling() {
        let chunk: ChatChunk = serde_json::from_str(
            r#"{"choices":[{"delta":{"reasoning_content":"structured","reasoning":"flat"}}]}"#,
        )
        .unwrap();
        assert_eq!(chunk.to_delta().reasoning_delta, Some("structured".into()));
        let flat: ChatChunk =
            serde_json::from_str(r#"{"choices":[{"delta":{"reasoning":"flat"}}]}"#).unwrap();
        assert_eq!(flat.to_delta().reasoning_delta, Some("flat".into()));
    }

    #[test]
    fn tool_call_fragments_keep_their_index_and_partial_arguments() {
        let chunk: ChatChunk = serde_json::from_str(
            r#"{"choices":[{"delta":{"tool_calls":[
                {"index":0,"id":"call_1","type":"function","function":{"name":"do_thing","arguments":"{\"x\":"}}
            ]}}]}"#,
        )
        .unwrap();
        let delta = chunk.to_delta();
        assert_eq!(
            delta.tool_calls,
            vec![ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("do_thing".into()),
                arguments_delta: Some("{\"x\":".into()),
            }]
        );
    }

    #[test]
    fn a_usage_only_chunk_carries_usage_and_no_output() {
        let chunk: ChatChunk = serde_json::from_str(
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13}}"#,
        )
        .unwrap();
        let delta = chunk.to_delta();
        assert_eq!(delta.content_delta, None);
        let usage = delta.usage.unwrap();
        assert_eq!((usage.prompt, usage.completion, usage.total), (10, 3, 13));
    }

    #[test]
    fn an_unknown_field_does_not_fail_the_chunk() {
        let chunk: ChatChunk = serde_json::from_str(
            r#"{"provider_extra":{"anything":1},"choices":[{"delta":{"content":"hi"}}]}"#,
        )
        .unwrap();
        assert_eq!(chunk.to_delta().content_delta, Some("hi".into()));
    }
}
