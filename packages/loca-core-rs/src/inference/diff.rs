//! Two successive partial parses in, one batch of stream deltas out.
//!
//! This is the whole of incremental streaming. The token loop parses the
//! accumulated text after every token (`is_partial = true`), and the difference
//! between that parse and the previous one IS the delta to emit; parsing only
//! at the end would produce the right final message and lose every incremental
//! tool-call and reasoning delta.
//!
//! [`diff_messages`] is a faithful port of llama.cpp's
//! `common_chat_msg_diff::compute_diffs` over `string_diff`, which is the
//! specification the tests below are written from. Ported rather than requested
//! across the shim because it is pure string and vector logic, so it stays in
//! the DEFAULT CI gate rather than behind an FFI boundary CI never builds.
//!
//! Three states a partial parser can reach are `Err` and fail the turn: fewer
//! tool calls than before, a tool name that is not an extension of the old one,
//! and a content string that is not an extension of the previous one. Resyncing
//! instead would keep the turn alive at the cost of duplicating text into the
//! PERSISTED message, hiding the degradation this stage exists to detect.
//! Nothing already emitted is lost: deltas reach the consumer before the error.

use crate::llm::{LlmDelta, ToolCallDelta};

use super::ids::ToolCallIds;
use super::message::ParsedMessage;

/// Why a diff could not be computed. Carried into a `ProviderError` by the
/// caller, which owns the mapping to a wire error code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffError(pub String);

impl std::fmt::Display for DiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

type Result<T> = std::result::Result<T, DiffError>;

/// The suffix `current` adds to `last`, including the case that looks like a
/// bug and is not: when `current` is a PREFIX of `last` the answer is the empty
/// string rather than an error, because the previous generation ended on a
/// partial stop word that had not been erased yet and this one ended on the
/// complete stop word, which was.
fn string_diff(last: &str, current: &str) -> Result<String> {
    if last.is_empty() {
        return Ok(current.to_string());
    }
    if let Some(suffix) = current.strip_prefix(last) {
        return Ok(suffix.to_string());
    }
    if last.starts_with(current) {
        return Ok(String::new());
    }
    Err(DiffError(format!(
        "invalid diff: {last:?} not found at start of {current:?}"
    )))
}

/// Every delta between two successive parses of one turn's output. `ids`
/// assigns and remembers a tool-call id per index, because llama.cpp leaves it
/// empty for most templates and the consumer replays it by id.
///
/// Deltas come back in upstream's order: reasoning, then content, then tool
/// calls, which is what a consumer rendering as it reads needs.
pub fn diff_messages(
    previous: &ParsedMessage,
    current: &ParsedMessage,
    ids: &mut ToolCallIds,
) -> Result<Vec<LlmDelta>> {
    let mut deltas = Vec::new();

    if previous.reasoning_content != current.reasoning_content {
        let delta = string_diff(&previous.reasoning_content, &current.reasoning_content)?;
        // A non-empty CHANGE can still yield an empty diff (the shrink case
        // above), and an empty delta is one the consumer must never see: it
        // would read as a token that was never generated.
        if !delta.is_empty() {
            deltas.push(LlmDelta {
                reasoning_delta: Some(delta),
                ..LlmDelta::default()
            });
        }
    }

    if previous.content != current.content {
        let delta = string_diff(&previous.content, &current.content)?;
        if !delta.is_empty() {
            deltas.push(LlmDelta {
                content_delta: Some(delta),
                ..LlmDelta::default()
            });
        }
    }

    if current.tool_calls.len() < previous.tool_calls.len() {
        return Err(DiffError(format!(
            "invalid diff: tool calls went from {} to {}",
            previous.tool_calls.len(),
            current.tool_calls.len(),
        )));
    }

    // The last call the previous parse had seen is the only OLD one that can
    // still be growing; everything before it was already complete. Upstream
    // diffs exactly that one, and diffing them all would re-emit finished
    // arguments on every token.
    if let Some(index) = previous.tool_calls.len().checked_sub(1) {
        let (Some(before), Some(after)) = (
            previous.tool_calls.get(index),
            current.tool_calls.get(index),
        ) else {
            return Err(DiffError(
                "invalid diff: the tool call being extended vanished".to_string(),
            ));
        };
        // The name may GROW during incremental parsing. One that changes to
        // something which is not an extension means the parse resynchronised
        // onto a different call, and continuing would attribute one call's
        // arguments to another's name.
        if before.name != after.name
            && !before.name.is_empty()
            && !after.name.is_empty()
            && !after.name.starts_with(&before.name)
        {
            return Err(DiffError(format!(
                "invalid diff: tool call name changed from {:?} to {:?}",
                before.name, after.name,
            )));
        }
        let arguments = string_diff(&before.arguments, &after.arguments)?;
        let identity_changed = before.id != after.id || before.name != after.name;
        if !arguments.is_empty() || identity_changed {
            // `index` is the position in the call list, which is the key the
            // consumer reassembles by (`llm::types::ToolCallDelta`).
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            deltas.push(LlmDelta {
                tool_calls: vec![ToolCallDelta {
                    index,
                    // Only on a change: resending identity with every argument
                    // fragment would differ from what `llama-server` streams.
                    id: identity_changed.then(|| ids.id_for(index, &after.id)),
                    name: identity_changed.then(|| after.name.clone()),
                    arguments_delta: (!arguments.is_empty()).then_some(arguments),
                }],
                ..LlmDelta::default()
            });
        }
    }

    // Calls that appeared in this parse are emitted whole.
    for (index, call) in current
        .tool_calls
        .iter()
        .enumerate()
        .skip(previous.tool_calls.len())
    {
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        deltas.push(LlmDelta {
            tool_calls: vec![ToolCallDelta {
                index,
                id: Some(ids.id_for(index, &call.id)),
                name: Some(call.name.clone()),
                // Absent rather than `Some("")`: an empty argument fragment is
                // not a fragment.
                arguments_delta: (!call.arguments.is_empty()).then(|| call.arguments.clone()),
            }],
            ..LlmDelta::default()
        });
    }

    Ok(deltas)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::inference::message::ParsedToolCall;

    fn msg(content: &str) -> ParsedMessage {
        ParsedMessage {
            content: content.to_string(),
            ..ParsedMessage::default()
        }
    }

    fn thinking(reasoning: &str, content: &str) -> ParsedMessage {
        ParsedMessage {
            content: content.to_string(),
            reasoning_content: reasoning.to_string(),
            ..ParsedMessage::default()
        }
    }

    fn call(name: &str, arguments: &str) -> ParsedToolCall {
        ParsedToolCall {
            name: name.to_string(),
            arguments: arguments.to_string(),
            id: String::new(),
        }
    }

    fn with_calls(calls: &[ParsedToolCall]) -> ParsedMessage {
        ParsedMessage {
            tool_calls: calls.to_vec(),
            ..ParsedMessage::default()
        }
    }

    fn diff(previous: &ParsedMessage, current: &ParsedMessage) -> Vec<LlmDelta> {
        diff_messages(previous, current, &mut ToolCallIds::new()).expect("a valid diff")
    }

    // --- string_diff, case by case from common/chat.cpp:56-69 ----------------

    #[test]
    fn an_empty_previous_yields_the_whole_current() {
        assert_eq!(string_diff("", "Hello").unwrap(), "Hello");
    }

    #[test]
    fn an_extension_yields_only_the_suffix() {
        assert_eq!(string_diff("Hello", "Hello world").unwrap(), " world");
    }

    #[test]
    fn a_shrink_yields_empty_rather_than_an_error() {
        // The previous generation ended on a partial stop word that was not
        // erased, and this one ended on the stop word, which was. Treating it
        // as an error would fail a turn that is ending NORMALLY.
        assert_eq!(string_diff("Hello wor", "Hello").unwrap(), "");
    }

    #[test]
    fn an_unrelated_string_is_an_error_naming_both_sides() {
        let err = string_diff("Hello", "Goodbye").unwrap_err();
        assert!(err.0.contains("Hello"), "{}", err.0);
        assert!(err.0.contains("Goodbye"), "{}", err.0);
    }

    // --- content and reasoning ------------------------------------------------

    #[test]
    fn content_growth_emits_only_the_new_text() {
        let deltas = diff(&msg("Hello"), &msg("Hello world"));
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].content_delta.as_deref(), Some(" world"));
    }

    #[test]
    fn an_unchanged_parse_emits_nothing_at_all() {
        // The common case once per token: the parser is holding a partial tag
        // and nothing observable moved. Emitting an empty delta here would put a
        // phantom token on the stream on most tokens of most generations.
        assert!(diff(&msg("Hello"), &msg("Hello")).is_empty());
    }

    #[test]
    fn a_shrink_changes_the_string_but_emits_no_delta() {
        // The two halves have to be checked together: the strings differ, so the
        // change IS detected, and the diff is empty, so nothing is emitted.
        // A version that emitted `Some("")` would pass a "no new text" check
        // while still putting an empty delta on the stream.
        let deltas = diff(&msg("Hello wor"), &msg("Hello"));
        assert!(
            deltas.is_empty(),
            "an erased partial stop word is not a token: {deltas:?}",
        );
    }

    #[test]
    fn reasoning_is_emitted_before_content_and_on_its_own_channel() {
        let deltas = diff(
            &thinking("Let me", ""),
            &thinking("Let me think", "The answer"),
        );
        assert_eq!(deltas.len(), 2);
        // Order is upstream's and is what llama-server streams. A consumer
        // rendering as it reads would otherwise show the answer before the
        // thinking that produced it.
        assert_eq!(deltas[0].reasoning_delta.as_deref(), Some(" think"));
        assert_eq!(deltas[0].content_delta, None);
        assert_eq!(deltas[1].content_delta.as_deref(), Some("The answer"));
        assert_eq!(deltas[1].reasoning_delta, None);
    }

    #[test]
    fn a_reasoning_shrink_changes_the_string_but_emits_no_delta() {
        // The reasoning twin of the content case above: two channels with the
        // same rule need the same test.
        let deltas = diff(&thinking("I was thin", ""), &thinking("I was", ""));
        assert!(
            deltas.is_empty(),
            "an erased partial stop word is not a thought: {deltas:?}",
        );
    }

    #[test]
    fn an_identity_only_change_emits_no_argument_fragment() {
        // The extend path's version of "an empty fragment is not a fragment",
        // reachable when a name finishes parsing on a token that added no
        // arguments.
        let deltas = diff(
            &with_calls(&[call("get_", "{}")]),
            &with_calls(&[call("get_weather", "{}")]),
        );
        assert_eq!(deltas.len(), 1);
        let fragment = &deltas[0].tool_calls[0];
        assert_eq!(fragment.name.as_deref(), Some("get_weather"));
        assert_eq!(
            fragment.arguments_delta, None,
            "identity moved and the arguments did not, so there is no argument \
             fragment to send",
        );
    }

    #[test]
    fn reasoning_never_leaks_into_content() {
        let deltas = diff(&ParsedMessage::default(), &thinking("thoughts", ""));
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].reasoning_delta.as_deref(), Some("thoughts"));
        assert_eq!(
            deltas[0].content_delta, None,
            "the deepseek split is the whole point of pinning the reasoning \
             format; merging the channels would put the model's thoughts in the \
             answer the user reads",
        );
    }

    // --- tool calls -----------------------------------------------------------

    #[test]
    fn a_new_tool_call_is_emitted_whole_with_an_id_and_a_name() {
        let deltas = diff(
            &ParsedMessage::default(),
            &with_calls(&[call("get_weather", "{\"city\":")]),
        );
        assert_eq!(deltas.len(), 1);
        let fragment = &deltas[0].tool_calls[0];
        assert_eq!(fragment.index, 0);
        assert_eq!(fragment.name.as_deref(), Some("get_weather"));
        assert_eq!(fragment.arguments_delta.as_deref(), Some("{\"city\":"));
        assert!(
            fragment.id.as_deref().is_some_and(|id| !id.is_empty()),
            "llama.cpp leaves the id empty for most templates and the consumer \
             replays the call by id",
        );
    }

    #[test]
    fn later_fragments_extend_the_arguments_and_repeat_neither_id_nor_name() {
        let previous = with_calls(&[call("get_weather", "{\"city\":")]);
        let current = with_calls(&[call("get_weather", "{\"city\":\"Jakarta\"}")]);
        let deltas = diff(&previous, &current);
        assert_eq!(deltas.len(), 1);
        let fragment = &deltas[0].tool_calls[0];
        assert_eq!(fragment.arguments_delta.as_deref(), Some("\"Jakarta\"}"));
        assert_eq!(
            (fragment.id.as_ref(), fragment.name.as_ref()),
            (None, None),
            "llama-server sends identity once and extends arguments after, and \
             the parity test compares against that",
        );
    }

    #[test]
    fn a_name_may_grow_but_may_not_change() {
        // Both halves of upstream's rule. Growth is ordinary incremental
        // parsing; a change means the parse resynchronised onto a different
        // call, and continuing would attribute these arguments to that name.
        let grown = diff(
            &with_calls(&[call("get_", "")]),
            &with_calls(&[call("get_weather", "")]),
        );
        assert_eq!(grown.len(), 1);
        assert_eq!(grown[0].tool_calls[0].name.as_deref(), Some("get_weather"));

        let err = diff_messages(
            &with_calls(&[call("get_weather", "")]),
            &with_calls(&[call("send_email", "")]),
            &mut ToolCallIds::new(),
        )
        .unwrap_err();
        assert!(err.0.contains("get_weather"), "{}", err.0);
        assert!(err.0.contains("send_email"), "{}", err.0);
    }

    #[test]
    fn a_name_appearing_from_empty_is_growth_not_a_change() {
        let deltas = diff(
            &with_calls(&[call("", "")]),
            &with_calls(&[call("get_weather", "")]),
        );
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].tool_calls[0].name.as_deref(), Some("get_weather"));
    }

    #[test]
    fn losing_a_tool_call_is_an_error() {
        let err = diff_messages(
            &with_calls(&[call("a", ""), call("b", "")]),
            &with_calls(&[call("a", "")]),
            &mut ToolCallIds::new(),
        )
        .unwrap_err();
        assert!(err.0.contains("2"), "{}", err.0);
        assert!(err.0.contains("1"), "{}", err.0);
    }

    #[test]
    fn a_second_call_appearing_does_not_re_emit_the_first() {
        // The reason upstream diffs only the LAST previously-seen call: the
        // finished ones must not have their arguments streamed again.
        let previous = with_calls(&[call("first", "{\"a\":1}")]);
        let current = with_calls(&[call("first", "{\"a\":1}"), call("second", "{\"b\":")]);
        let deltas = diff(&previous, &current);
        assert_eq!(deltas.len(), 1, "only the new call: {deltas:?}");
        let fragment = &deltas[0].tool_calls[0];
        assert_eq!(fragment.index, 1);
        assert_eq!(fragment.name.as_deref(), Some("second"));
    }

    #[test]
    fn two_calls_appearing_at_once_keep_their_indices() {
        let deltas = diff(
            &ParsedMessage::default(),
            &with_calls(&[call("first", "{}"), call("second", "{}")]),
        );
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].tool_calls[0].index, 0);
        assert_eq!(deltas[1].tool_calls[0].index, 1);
    }

    #[test]
    fn a_call_with_no_arguments_yet_emits_no_argument_fragment() {
        let deltas = diff(&ParsedMessage::default(), &with_calls(&[call("f", "")]));
        assert_eq!(
            deltas[0].tool_calls[0].arguments_delta, None,
            "an empty argument fragment is not a fragment, and the consumer \
             concatenates these",
        );
    }

    #[test]
    fn ids_are_stable_across_the_fragments_of_one_call() {
        // The consumer reassembles by index and replays the call by id. An id
        // that changed mid-call would produce a `tool_call_id` on the transcript
        // that matches no call.
        let mut ids = ToolCallIds::new();
        let first = diff_messages(
            &ParsedMessage::default(),
            &with_calls(&[call("f", "{")]),
            &mut ids,
        )
        .unwrap();
        let id = first[0].tool_calls[0]
            .id
            .clone()
            .expect("the first fragment carries the id");
        // A later parse where the NAME also grows, so identity is re-sent and
        // the id has to be the remembered one rather than a fresh one.
        let second = diff_messages(
            &with_calls(&[call("f", "{")]),
            &with_calls(&[call("fetch", "{}")]),
            &mut ids,
        )
        .unwrap();
        assert_eq!(second[0].tool_calls[0].id, Some(id));
    }

    #[test]
    fn a_model_supplied_id_is_kept_rather_than_replaced() {
        let mut ids = ToolCallIds::new();
        let deltas = diff_messages(
            &ParsedMessage::default(),
            &with_calls(&[ParsedToolCall {
                name: "f".to_string(),
                arguments: "{}".to_string(),
                id: "call_abc".to_string(),
            }]),
            &mut ids,
        )
        .unwrap();
        assert_eq!(deltas[0].tool_calls[0].id.as_deref(), Some("call_abc"));
    }

    // --- the parity property: this must match what D3's consumer was built on -

    /// One scripted generation through two paths, asserting identical deltas.
    /// The local engine emits no SSE, so what can be checked is that its delta
    /// sequence is indistinguishable from the one `SseDecoder` +
    /// `ChatChunk::to_delta` produce from the equivalent `llama-server` stream.
    /// Path B below is that real production code, not a model of it.
    #[test]
    fn the_emitted_deltas_match_what_the_sse_path_would_have_produced() {
        use crate::llm::sse::{ChatChunk, SseDecoder};

        // Successive partial parses, as the token loop would see them.
        let snapshots = vec![
            thinking("Let me", ""),
            thinking("Let me think", ""),
            thinking("Let me think", "It is"),
            thinking("Let me think", "It is 31 degrees."),
        ];

        // Path A: this module.
        let mut ids = ToolCallIds::new();
        let mut mine = Vec::new();
        let mut previous = ParsedMessage::default();
        for snapshot in &snapshots {
            mine.extend(diff_messages(&previous, snapshot, &mut ids).expect("a valid diff"));
            previous = snapshot.clone();
        }

        // Path B: the same content as llama-server-shaped SSE, decoded by D3.
        let feed = concat!(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Let me\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\" think\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"It is\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" 31 degrees.\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut decoder = SseDecoder::new();
        decoder.push(feed.as_bytes());
        let mut theirs = Vec::new();
        while let Some(event) = decoder.next_event() {
            if event == crate::llm::sse::DONE_SENTINEL {
                break;
            }
            let chunk: ChatChunk = serde_json::from_str(&event).expect("a fixture chunk");
            let delta = chunk.to_delta();
            // The SSE path drops empty deltas rather than yielding them, and so
            // must this module: that is the property, not the fixture.
            if !delta.is_empty() {
                theirs.push(delta);
            }
        }

        assert_eq!(
            mine, theirs,
            "the in-process engine must be indistinguishable from the SSE path \
             the consumer was written against",
        );
    }

    #[test]
    fn the_parity_property_can_fail() {
        // A parity assertion is worthless if both sides can drift together, so
        // this pins that the comparison discriminates: path B gets a stream
        // differing exactly as a regression would.
        use crate::llm::sse::ChatChunk;

        let chunk: ChatChunk =
            serde_json::from_str(r#"{"choices":[{"delta":{"role":"assistant","content":""}}]}"#)
                .expect("a fixture chunk");
        let empty = chunk.to_delta();
        assert!(
            empty.is_empty(),
            "the opening role chunk must produce nothing, which is what makes \
             the parity comparison meaningful",
        );
        let real = diff(&ParsedMessage::default(), &msg("Hello"));
        assert_ne!(
            vec![empty],
            real,
            "an empty delta and a real one must not compare equal",
        );
    }
}
