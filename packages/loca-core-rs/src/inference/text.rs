//! Token bytes to text, one token at a time.
//!
//! A token is not a character. `Model::token_bytes` returns BYTES because a
//! single token can be half a multi-byte character, and llama.cpp's byte-fallback
//! vocabulary makes that ordinary rather than exotic: any CJK, Cyrillic, Arabic
//! or emoji output is routinely split across two or three tokens.
//!
//! So this is the one place a visible corruption can be introduced. Converting
//! each token's bytes on their own (with `String::from_utf8_lossy`, which is the
//! obvious spelling) puts a replacement character in the middle of every
//! multi-byte character on those scripts, and the tests still pass because
//! English is one byte per character. The rule is: **complete characters go out,
//! an incomplete tail waits for the next token.**
//!
//! Bytes that can never complete are a different case and are NOT held: a lone
//! continuation byte is invalid rather than partial, and waiting for it to
//! become valid would stall the stream for the rest of the turn.

/// Accumulates token bytes and exposes the text that is complete.
#[derive(Debug, Default)]
pub struct Utf8Accumulator {
    text: String,
    /// The trailing bytes that do not yet form a character. Never more than
    /// three, since that is the longest incomplete UTF-8 sequence.
    pending: Vec<u8>,
}

impl Utf8Accumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one token's bytes, extending [`Utf8Accumulator::text`] with whatever
    /// they complete.
    pub fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    self.text.push_str(valid);
                    self.pending.clear();
                    return;
                }
                Err(err) => {
                    let good = err.valid_up_to();
                    // `from_utf8` on a `&[u8]` this function already validated
                    // up to `good` cannot fail, but the fallible form is used
                    // anyway: this module must not be the one place a panic can
                    // reach the token loop.
                    if let Ok(valid) = std::str::from_utf8(&self.pending[..good]) {
                        self.text.push_str(valid);
                    }
                    match err.error_len() {
                        // Genuinely invalid, not merely unfinished. Emitting the
                        // replacement character and moving on is what
                        // `from_utf8_lossy` does; holding the bytes instead
                        // would stall every later character behind a byte that
                        // can never complete.
                        Some(bad) => {
                            self.text.push(char::REPLACEMENT_CHARACTER);
                            self.pending.drain(..good + bad);
                        }
                        // An incomplete tail: keep it for the next token.
                        None => {
                            self.pending.drain(..good);
                            return;
                        }
                    }
                }
            }
        }
    }

    /// The complete text so far. Never ends mid-character.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Cut the text back to `len` bytes, which is what a completed stop string
    /// does: the marker and everything after it are not part of the answer.
    ///
    /// A length that is not a character boundary leaves the text alone rather
    /// than panicking. `String::truncate` panics there, and the one caller takes
    /// its index from `str::find`, so this cannot happen today; a `panic!`
    /// reachable from the token loop is not worth leaving to a future caller's
    /// arithmetic.
    pub fn truncate(&mut self, len: usize) {
        if self.text.is_char_boundary(len) {
            self.text.truncate(len);
        }
        // Whatever was still forming belonged to the discarded tail.
        self.pending.clear();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_character_split_across_tokens_arrives_whole_and_not_as_two_question_marks() {
        // The failure this module exists for. "日" is three bytes and llama.cpp
        // will hand them over in two or three pieces; converting each piece on
        // its own yields replacement characters, and every test written in
        // English passes anyway.
        let mut acc = Utf8Accumulator::new();
        let bytes = "日本語".as_bytes();
        for chunk in bytes.chunks(2) {
            acc.push(chunk);
            assert!(
                !acc.text().contains(char::REPLACEMENT_CHARACTER),
                "a partial character must be held, never rendered: {:?}",
                acc.text(),
            );
        }
        assert_eq!(acc.text(), "日本語");
    }

    #[test]
    fn an_incomplete_tail_is_held_back_rather_than_streamed() {
        // The other half, stated as the property the token loop depends on: what
        // `text()` returns is always something a client can render.
        let mut acc = Utf8Accumulator::new();
        acc.push(b"ok");
        acc.push(&"日".as_bytes()[..1]);
        assert_eq!(
            acc.text(),
            "ok",
            "the first byte of a character is not text"
        );
        acc.push(&"日".as_bytes()[1..]);
        assert_eq!(acc.text(), "ok日");
    }

    #[test]
    fn an_emoji_arriving_byte_by_byte_is_one_character_at_the_end() {
        let mut acc = Utf8Accumulator::new();
        for byte in "🙂".as_bytes() {
            acc.push(&[*byte]);
        }
        assert_eq!(acc.text(), "🙂");
        assert_eq!(acc.text().chars().count(), 1);
    }

    #[test]
    fn a_byte_that_can_never_complete_does_not_stall_the_stream() {
        // A lone continuation byte is invalid, not partial. Holding it would
        // mean everything generated after it never reaches the client, which is
        // a hang rather than a glyph.
        let mut acc = Utf8Accumulator::new();
        acc.push(&[0x80]);
        acc.push(b"after");
        assert!(acc.text().ends_with("after"), "{:?}", acc.text());
        assert!(acc.text().starts_with(char::REPLACEMENT_CHARACTER));
    }

    #[test]
    fn truncating_drops_the_tail_and_anything_still_forming() {
        let mut acc = Utf8Accumulator::new();
        acc.push("answer<|im".as_bytes());
        acc.push(&"日".as_bytes()[..1]);
        acc.truncate("answer".len());
        assert_eq!(acc.text(), "answer");
        // And the held byte is gone with it: keeping it would prepend half a
        // character to whatever the next turn generates.
        // Two replacement characters, one per orphaned continuation byte: the
        // lead byte they belonged to went with the truncation, which is the
        // point. Keeping it instead would splice half a character onto whatever
        // came next.
        acc.push(&"日".as_bytes()[1..]);
        assert_eq!(acc.text(), "answer\u{fffd}\u{fffd}");
    }

    #[test]
    fn truncating_off_a_character_boundary_leaves_the_text_alone() {
        // Unreachable from the token loop, whose index comes from `str::find`.
        // It is here because the alternative spelling is `String::truncate`,
        // which panics, and a panic in this module reaches the middle of a turn.
        let mut acc = Utf8Accumulator::new();
        acc.push("日本".as_bytes());
        acc.truncate(1);
        assert_eq!(acc.text(), "日本");
    }

    #[test]
    fn plain_text_passes_through_unchanged() {
        let mut acc = Utf8Accumulator::new();
        acc.push(b"It is ");
        acc.push(b"31 degrees.");
        assert_eq!(acc.text(), "It is 31 degrees.");
    }
}
