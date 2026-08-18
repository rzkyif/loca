//! Stop strings, and the suffix that must not be streamed yet.
//!
//! A turn ends on an end-of-generation token, on the token cap, or on a stop
//! STRING. The last one is the awkward member: a stop string is text, tokens do
//! not align to it, and the model can emit the first half of one and then
//! continue with something else entirely.
//!
//! `common_chat_templates_apply` returns the stops the template itself declares
//! (`AppliedChat::additional_stops`), so most turns have at least one.
//!
//! # The half-emitted stop string
//!
//! If the accumulated text ends with a PREFIX of a stop string, that suffix
//! cannot be streamed: it is either about to become a stop (and must never be
//! shown) or about to become ordinary text (and must be shown, once we know).
//! [`safe_len`] is where the line is drawn, and holding that suffix back is what
//! makes the "erased partial stop word" case in [`super::diff`] reachable, which
//! is why upstream's `string_diff` tolerates the text getting shorter.
//!
//! Getting this wrong is not subtle in either direction: emit too eagerly and a
//! stop marker like `<|im_end|>` appears in the answer; hold too much back and
//! the last few characters of every reply never arrive.

/// What the accumulated text means for the loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopScan {
    /// No stop string is present or forming. Everything up to `safe` bytes may
    /// be streamed; the remainder is a partial stop and is held.
    Continue { safe: usize },
    /// A stop string completed. The turn ends and only `keep` bytes are kept:
    /// the stop text itself is never part of the answer.
    Hit { keep: usize },
}

/// Scan `text` for a completed or forming stop string.
///
/// The FIRST completed stop wins by position, not by the order of `stops`: a
/// later-listed stop appearing earlier in the text still ends the turn there.
/// Scanning by list order instead would let text past a stop marker reach the
/// transcript whenever two stops are configured.
pub fn scan(text: &str, stops: &[String]) -> StopScan {
    let mut hit: Option<usize> = None;
    for stop in stops.iter().filter(|s| !s.is_empty()) {
        if let Some(at) = text.find(stop.as_str()) {
            hit = Some(hit.map_or(at, |current: usize| current.min(at)));
        }
    }
    if let Some(keep) = hit {
        return StopScan::Hit { keep };
    }
    StopScan::Continue {
        safe: safe_len(text, stops),
    }
}

/// How much of `text` is safe to stream, given that a stop string may be half
/// emitted at the end.
///
/// Returns the length of `text` minus the longest suffix that is a proper prefix
/// of some stop string. Suffixes are tested longest-first, and only at character
/// boundaries: slicing a multi-byte character in half to compare it would panic,
/// and this runs on every token of every generation.
fn safe_len(text: &str, stops: &[String]) -> usize {
    let mut longest = 0;
    for stop in stops.iter().filter(|s| !s.is_empty()) {
        // A suffix as long as the stop itself would be a full match, which
        // `scan` has already ruled out, so start one shorter.
        let max = stop.len().saturating_sub(1).min(text.len());
        for len in (1..=max).rev() {
            let Some(start) = text.len().checked_sub(len) else {
                continue;
            };
            if !text.is_char_boundary(start) {
                continue;
            }
            if stop.starts_with(&text[start..]) {
                longest = longest.max(len);
                break;
            }
        }
    }
    text.len() - longest
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn stops(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn plain_text_with_no_stops_is_entirely_safe() {
        assert_eq!(
            scan("Hello world", &[]),
            StopScan::Continue { safe: 11 },
            "with nothing to hold back, holding anything back would strand the \
             tail of every reply",
        );
    }

    #[test]
    fn a_completed_stop_ends_the_turn_and_is_not_kept() {
        let text = "The answer is 31.<|im_end|>";
        assert_eq!(
            scan(text, &stops(&["<|im_end|>"])),
            StopScan::Hit { keep: 17 },
        );
        assert_eq!(&text[..17], "The answer is 31.");
    }

    #[test]
    fn a_forming_stop_is_held_back_rather_than_streamed() {
        // The whole reason this module exists. "<|im" is three characters that
        // are about to become either a stop marker or ordinary text, and there
        // is no way to know yet.
        assert_eq!(
            scan("The answer is 31.<|im", &stops(&["<|im_end|>"])),
            StopScan::Continue { safe: 17 },
        );
    }

    #[test]
    fn a_forming_stop_that_turns_out_to_be_text_is_released() {
        // The other half, and the one a naive implementation gets wrong by
        // holding the text forever. Once the continuation rules the stop out,
        // everything is safe again.
        assert_eq!(
            scan("The answer is 31.<|imagine", &stops(&["<|im_end|>"])),
            StopScan::Continue { safe: 26 },
        );
    }

    #[test]
    fn the_earliest_stop_wins_not_the_first_listed() {
        // Two stops where the SECOND-listed one appears first. Scanning in list
        // order would keep 20 bytes and let the text between the two markers
        // reach the transcript.
        let text = "answer<|eot|>more<|im_end|>";
        assert_eq!(
            scan(text, &stops(&["<|im_end|>", "<|eot|>"])),
            StopScan::Hit { keep: 6 },
        );
        assert_eq!(&text[..6], "answer");
    }

    #[test]
    fn a_stop_at_the_very_start_keeps_nothing() {
        assert_eq!(
            scan("<|im_end|>", &stops(&["<|im_end|>"])),
            StopScan::Hit { keep: 0 },
        );
    }

    #[test]
    fn an_empty_stop_string_is_ignored_rather_than_matching_everywhere() {
        // An empty stop matches at position 0 of any text, so failing to filter
        // it would end every turn before its first token with no diagnosis.
        assert_eq!(
            scan("Hello", &stops(&["", "<|im_end|>"])),
            StopScan::Continue { safe: 5 },
        );
    }

    #[test]
    fn the_longest_forming_stop_is_the_one_held() {
        // Two stops sharing a prefix. Holding the shorter match would stream a
        // character that belongs to the longer stop.
        assert_eq!(
            scan("text<|e", &stops(&["<|eot|>", "<|e|>"])),
            StopScan::Continue { safe: 4 },
        );
    }

    #[test]
    fn a_multi_byte_character_at_the_boundary_does_not_panic() {
        // `safe_len` slices by byte offset, so a suffix test landing inside a
        // multi-byte character would panic. This runs on every token of every
        // generation, and CJK and emoji are ordinary content.
        let text = "答えは31です";
        assert_eq!(
            scan(text, &stops(&["す<|im_end|>"])),
            StopScan::Continue {
                safe: text.len() - "す".len()
            },
            "the held suffix must start on a character boundary",
        );
        // And the same text with a stop it cannot form at all is fully safe.
        assert_eq!(
            scan(text, &stops(&["<|im_end|>"])),
            StopScan::Continue { safe: text.len() },
        );
    }

    #[test]
    fn an_emoji_tail_is_streamed_rather_than_held() {
        let text = "done 🎉";
        assert_eq!(
            scan(text, &stops(&["<|im_end|>"])),
            StopScan::Continue { safe: text.len() },
        );
    }
}
