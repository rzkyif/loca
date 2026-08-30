//! Token bytes to text. A token can be half a multi-byte character, so complete
//! characters go out and an incomplete tail waits for the next token. Bytes that
//! can never complete are replaced rather than held, or the stream would stall.

/// Accumulates token bytes and exposes the text that is complete.
#[derive(Debug, Default)]
pub struct Utf8Accumulator {
    text: String,
    /// Trailing bytes that do not yet form a character (at most three).
    pending: Vec<u8>,
}

impl Utf8Accumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one token's bytes, extending the text with whatever they complete.
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
                    // Cannot fail, but this module must never panic.
                    if let Ok(valid) = std::str::from_utf8(&self.pending[..good]) {
                        self.text.push_str(valid);
                    }
                    match err.error_len() {
                        // Genuinely invalid, not unfinished.
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

    /// Cut the text back to `len` bytes (a completed stop string). A length off
    /// a character boundary leaves the text alone rather than panicking.
    pub fn truncate(&mut self, len: usize) {
        if self.text.is_char_boundary(len) {
            self.text.truncate(len);
        }
        self.pending.clear();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_character_split_across_tokens_arrives_whole_and_not_as_two_question_marks() {
        let mut acc = Utf8Accumulator::new();
        let bytes = "日本語".as_bytes();
        for chunk in bytes.chunks(2) {
            acc.push(chunk);
            assert!(
                !acc.text().contains(char::REPLACEMENT_CHARACTER),
                "{:?}",
                acc.text()
            );
        }
        assert_eq!(acc.text(), "日本語");
    }

    #[test]
    fn an_incomplete_tail_is_held_back_rather_than_streamed() {
        let mut acc = Utf8Accumulator::new();
        acc.push(b"ok");
        acc.push(&"日".as_bytes()[..1]);
        assert_eq!(acc.text(), "ok");
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
        // Two replacement characters: the lead byte went with the truncation.
        acc.push(&"日".as_bytes()[1..]);
        assert_eq!(acc.text(), "answer\u{fffd}\u{fffd}");
    }

    #[test]
    fn truncating_off_a_character_boundary_leaves_the_text_alone() {
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
