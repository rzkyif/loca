//! Bounds on the display payloads a tool pushes into chat.
//!
//! Ports `packages/loca-core/src/extensions/worker-display-bounds.ts`.
//!
//! Accepted content is persisted into the session file, which is rewritten on
//! every later message, and broadcast to every paired client. So the bound is
//! not decoration: an unbounded payload is re-serialized on every subsequent
//! turn of the conversation and pushed down every open socket again.
//!
//! # Truncate versus reject is per-kind, and deliberately not uniform
//!
//! Markdown is TRUNCATED with a marker, because half a document still reads as
//! the document and the tool's intent survives. The other three are REJECTED,
//! because truncating them changes what they mean rather than how much of them
//! there is: half a base64 image is a broken image, a table missing its last
//! rows silently misreports its own data, and a diff with one side clipped
//! shows a change that was never made. A rewrite that tidies this into one
//! uniform rule loses that distinction in the direction of showing the user
//! something false.

use crate::contract::api::DisplayContent;

/// Markdown, and each side of a diff.
const DISPLAY_TEXT_MAX_CHARS: usize = 256_000;
/// The base64 text of an image, not its decoded byte length.
const DISPLAY_IMAGE_MAX_B64_CHARS: usize = 8_000_000;
const DISPLAY_TABLE_MAX_ROWS: usize = 1_000;
const DISPLAY_TABLE_MAX_COLUMNS: usize = 64;
const DISPLAY_TABLE_MAX_CELL_CHARS: usize = 4_096;

const TRUNCATION_MARKER: &str = "\n\n[truncated]";

/// Bound one display payload.
///
/// `Ok` carries the content to persist, truncated if it was markdown over the
/// cap. `Err` carries the message the caller logs and shows the tool, and
/// nothing is persisted.
///
/// # Counting
///
/// JavaScript's `.length` is UTF-16 code units; this counts Unicode scalar
/// values, so an astral-plane character (an emoji, most CJK extensions) counts
/// once here and twice there. Nothing observable turns on the difference:
/// these are size bounds on content that never round-trips through a length,
/// and the divergence needs a payload within one character of a 256,000-char
/// cap to be reachable at all. Counting scalars is what lets the truncation
/// below split on a character boundary, which is the property that actually
/// matters, since slicing UTF-8 anywhere else does not produce a string.
pub fn bound_display_content(content: DisplayContent) -> Result<DisplayContent, String> {
    match content {
        DisplayContent::Markdown { markdown } => {
            if count(&markdown) > DISPLAY_TEXT_MAX_CHARS {
                let head: String = markdown.chars().take(DISPLAY_TEXT_MAX_CHARS).collect();
                return Ok(DisplayContent::Markdown {
                    markdown: head + TRUNCATION_MARKER,
                });
            }
            Ok(DisplayContent::Markdown { markdown })
        }
        DisplayContent::Image { data_b64, .. }
            if count(&data_b64) > DISPLAY_IMAGE_MAX_B64_CHARS =>
        {
            Err(format!(
                "image exceeds {DISPLAY_IMAGE_MAX_B64_CHARS} base64 characters"
            ))
        }
        DisplayContent::Table { columns, rows } => {
            if columns.len() > DISPLAY_TABLE_MAX_COLUMNS {
                return Err(format!("table exceeds {DISPLAY_TABLE_MAX_COLUMNS} columns"));
            }
            if rows.len() > DISPLAY_TABLE_MAX_ROWS {
                return Err(format!("table exceeds {DISPLAY_TABLE_MAX_ROWS} rows"));
            }
            let oversize = columns
                .iter()
                .chain(rows.iter().flatten())
                .any(|cell| count(cell) > DISPLAY_TABLE_MAX_CELL_CHARS);
            if oversize {
                return Err(format!(
                    "table cell exceeds {DISPLAY_TABLE_MAX_CELL_CHARS} characters"
                ));
            }
            Ok(DisplayContent::Table { columns, rows })
        }
        DisplayContent::Diff {
            before,
            after,
            title,
        } => {
            if count(&before) > DISPLAY_TEXT_MAX_CHARS || count(&after) > DISPLAY_TEXT_MAX_CHARS {
                return Err(format!(
                    "diff side exceeds {DISPLAY_TEXT_MAX_CHARS} characters"
                ));
            }
            Ok(DisplayContent::Diff {
                before,
                after,
                title,
            })
        }
        other => Ok(other),
    }
}

fn count(s: &str) -> usize {
    s.chars().count()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn markdown(chars: usize) -> DisplayContent {
        DisplayContent::Markdown {
            markdown: "m".repeat(chars),
        }
    }

    fn body(content: &DisplayContent) -> &str {
        match content {
            DisplayContent::Markdown { markdown } => markdown,
            _ => panic!("not markdown"),
        }
    }

    #[test]
    fn markdown_at_the_cap_is_passed_through_untouched() {
        // Literal 256,000, not `DISPLAY_TEXT_MAX_CHARS`. A fixture computed
        // from the constant scales with any mutation of it and cannot see the
        // cap move, which is exactly how two of this package's phase-1
        // mutations survived their tests.
        let content = bound_display_content(markdown(256_000)).unwrap();
        assert_eq!(body(&content).len(), 256_000);
        assert!(!body(&content).contains("[truncated]"));
    }

    #[test]
    fn markdown_over_the_cap_is_truncated_rather_than_dropped() {
        let content = bound_display_content(markdown(256_001)).unwrap();
        let out = body(&content);
        assert!(out.ends_with(TRUNCATION_MARKER), "no truncation marker");
        assert_eq!(
            out.chars().count(),
            256_000 + TRUNCATION_MARKER.chars().count()
        );
    }

    #[test]
    fn the_markdown_cap_is_two_hundred_and_fifty_six_thousand() {
        // Both directions, so neither halving nor doubling the constant
        // survives. Halving alone is caught by the pass-through test; doubling
        // alone is caught by this one.
        assert!(
            bound_display_content(markdown(128_001))
                .map(|c| body(&c).to_string())
                .is_ok_and(|m| !m.contains("[truncated]")),
            "a cap of 128,000 would have truncated this"
        );
        let big = bound_display_content(markdown(512_000)).unwrap();
        assert!(
            body(&big).contains("[truncated]"),
            "a cap of 512,000 would have let this through"
        );
    }

    #[test]
    fn truncation_splits_on_a_character_boundary() {
        // The 256,000th character is multi-byte, so a byte slice at that offset
        // would not be a string at all. This is why the count is scalars.
        let markdown: String = std::iter::repeat_n('e', 255_999)
            .chain(['\u{1F600}'])
            .collect();
        let content = bound_display_content(DisplayContent::Markdown {
            markdown: markdown.clone() + "tail",
        })
        .unwrap();
        assert!(body(&content).ends_with(&("\u{1F600}".to_string() + TRUNCATION_MARKER)));
    }

    #[test]
    fn an_oversize_image_is_rejected_rather_than_clipped() {
        // A clipped base64 payload decodes to a corrupt image, so the caller
        // must see an error rather than a picture that will not render.
        let over = DisplayContent::Image {
            alt: None,
            data_b64: "A".repeat(8_000_001),
            mime: "image/png".to_string(),
        };
        let err = bound_display_content(over).unwrap_err();
        assert!(err.contains("8000000"), "unexpected message: {err}");

        let at_cap = DisplayContent::Image {
            alt: None,
            data_b64: "A".repeat(8_000_000),
            mime: "image/png".to_string(),
        };
        assert!(bound_display_content(at_cap).is_ok(), "the cap moved");
    }

    #[test]
    fn a_table_is_bounded_on_all_three_axes_separately() {
        // Three separate caps with three separate messages: a table that is
        // wide, one that is long, and one with a single enormous cell fail for
        // different reasons and a user needs to know which.
        let wide = DisplayContent::Table {
            columns: (0..65).map(|i| i.to_string()).collect(),
            rows: vec![],
        };
        assert!(bound_display_content(wide).unwrap_err().contains("columns"));

        let long = DisplayContent::Table {
            columns: vec!["a".to_string()],
            rows: vec![vec!["x".to_string()]; 1_001],
        };
        assert!(bound_display_content(long).unwrap_err().contains("rows"));

        let fat_cell = DisplayContent::Table {
            columns: vec!["a".to_string()],
            rows: vec![vec!["x".repeat(4_097)]],
        };
        assert!(bound_display_content(fat_cell)
            .unwrap_err()
            .contains("cell"));

        // A HEADER cell counts too. The TypeScript checks both and it is the
        // easy half to drop, because a table's bulk is always in its rows.
        let fat_header = DisplayContent::Table {
            columns: vec!["h".repeat(4_097)],
            rows: vec![],
        };
        assert!(bound_display_content(fat_header)
            .unwrap_err()
            .contains("cell"));

        let ok = DisplayContent::Table {
            columns: (0..64).map(|i| i.to_string()).collect(),
            rows: vec![vec!["x".repeat(4_096); 64]; 1_000],
        };
        assert!(bound_display_content(ok).is_ok(), "a cap moved");
    }

    #[test]
    fn a_diff_is_rejected_rather_than_truncated_on_either_side() {
        // Truncating a diff shows the user a change that was never made, which
        // is worse than showing nothing. Both sides are checked, and the
        // `after` side is the one a port that only reads `before` would drop.
        for (before, after) in [(256_001, 1), (1, 256_001)] {
            let content = DisplayContent::Diff {
                before: "b".repeat(before),
                after: "a".repeat(after),
                title: None,
            };
            assert!(bound_display_content(content)
                .unwrap_err()
                .contains("diff side"),);
        }
        let ok = DisplayContent::Diff {
            before: "b".repeat(256_000),
            after: "a".repeat(256_000),
            title: Some("t".to_string()),
        };
        assert!(bound_display_content(ok).is_ok());
    }

    #[test]
    fn an_ordinary_payload_of_every_kind_survives_unchanged() {
        // The positive control. Without it a function that rejected everything
        // would pass every rejection test above.
        let kinds = [
            DisplayContent::Markdown {
                markdown: "# hi".to_string(),
            },
            DisplayContent::Image {
                alt: Some("a cat".to_string()),
                data_b64: "AAAA".to_string(),
                mime: "image/png".to_string(),
            },
            DisplayContent::Table {
                columns: vec!["a".to_string()],
                rows: vec![vec!["1".to_string()]],
            },
            DisplayContent::Diff {
                before: "x".to_string(),
                after: "y".to_string(),
                title: None,
            },
        ];
        for kind in kinds {
            let before = serde_json::to_value(&kind).unwrap();
            let after = serde_json::to_value(bound_display_content(kind).unwrap()).unwrap();
            assert_eq!(before, after);
        }
    }
}
