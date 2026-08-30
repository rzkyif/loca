//! Splitting an image-bearing transcript into a marker-carrying prompt and the
//! image bytes, for the local multimodal path.
//!
//! Pure: no shim types, no llama.cpp, no settings, so the decisions here are
//! tested in the DEFAULT gate while only the eval that consumes them is gated.
//!
//! An external provider takes images in-band; llama.cpp does not. The chat
//! template renders TEXT, and libmtmd needs the rendered prompt to contain a
//! marker at each position an image belongs so it can splice the encoded
//! embeddings between the surrounding text spans. So the local path replaces
//! each image part with the marker, applies the template to the now text-only
//! transcript, and hands the prompt and the image bytes to mtmd together.
//!
//! Order is the contract: mtmd pairs the Nth marker with the Nth bitmap, so the
//! extraction walks messages and parts in transcript order and never reorders
//! or deduplicates, even for two identical images.

use base64::Engine as _;

use crate::error::{validation, Result};
use crate::llm::{ChatMessage, ContentPart, LlmRequest, UserContent};

/// A transcript with its images lifted out.
#[derive(Clone, Debug)]
pub struct SplitPrompt {
    /// The transcript with every image part replaced by the marker text.
    pub messages: Vec<ChatMessage>,
    /// The image files, in transcript order, one per marker.
    pub images: Vec<Vec<u8>>,
}

impl SplitPrompt {
    pub fn has_images(&self) -> bool {
        !self.images.is_empty()
    }
}

/// How many images this request carries, without decoding any of them. Cheap
/// enough to call on every turn, which is what lets the engine choose between
/// the text path and the multimodal one.
pub fn image_count(req: &LlmRequest) -> usize {
    req.messages
        .iter()
        .map(|m| match m {
            ChatMessage::User {
                content: UserContent::Parts(parts),
            } => parts
                .iter()
                .filter(|p| matches!(p, ContentPart::ImageUrl { .. }))
                .count(),
            _ => 0,
        })
        .sum()
}

/// Replace every image part with `marker` and return the decoded image bytes.
/// `marker` comes from the loaded projector rather than being hardcoded: it is
/// model-family specific, and the wrong one silently detaches the image.
pub fn split(req: &LlmRequest, marker: &str) -> Result<SplitPrompt> {
    let mut images = Vec::new();
    let mut messages = Vec::with_capacity(req.messages.len());

    for message in &req.messages {
        let ChatMessage::User {
            content: UserContent::Parts(parts),
        } = message
        else {
            messages.push(message.clone());
            continue;
        };

        // The parts collapse to one string, which is what the template renders
        // and mtmd then scans. Newline-joined so a caption and its image do not
        // run together into one word.
        let mut text = String::new();
        for part in parts {
            match part {
                ContentPart::Text { text: t } => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(t);
                }
                ContentPart::ImageUrl { image_url } => {
                    images.push(decode_image(&image_url.url, images.len())?);
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(marker);
                }
            }
        }
        messages.push(ChatMessage::User {
            content: UserContent::Text(text),
        });
    }

    Ok(SplitPrompt { messages, images })
}

/// Decode one `data:` URI into its bytes. Only `data:` is accepted, and an
/// `https:` URL is REFUSED rather than fetched: that would be a network request
/// the user never asked for, on behalf of whatever put a URL in a message. The
/// client inlines attachments as data URIs already.
fn decode_image(url: &str, index: usize) -> Result<Vec<u8>> {
    let Some(rest) = url.strip_prefix("data:") else {
        return Err(validation(format!(
            "image {index} is a remote URL, which a local model cannot use: the core will not \
             fetch it. Attach the image instead, or use an external provider."
        )));
    };
    // `data:[<mediatype>][;base64],<data>`. The media type is not inspected:
    // mtmd sniffs the real format from the bytes.
    let Some((meta, payload)) = rest.split_once(',') else {
        return Err(validation(format!(
            "image {index} is a malformed data URI: no comma separating the metadata from the \
             payload"
        )));
    };
    if !meta.ends_with(";base64") && !meta.eq_ignore_ascii_case("base64") {
        return Err(validation(format!(
            "image {index} is a data URI that is not base64-encoded, which no client produces"
        )));
    }
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|e| validation(format!("image {index} is not valid base64: {e}")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::llm::{ImageUrl, LlmEndpointConfig};

    const MARKER: &str = "<__media__>";

    /// A one-pixel PNG, base64'd, as a data URI.
    fn data_uri(bytes: &[u8]) -> String {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    fn req(messages: Vec<ChatMessage>) -> LlmRequest {
        LlmRequest::new(LlmEndpointConfig::default(), messages)
    }

    fn parts(parts: Vec<ContentPart>) -> ChatMessage {
        ChatMessage::User {
            content: UserContent::Parts(parts),
        }
    }

    fn image(bytes: &[u8]) -> ContentPart {
        ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: data_uri(bytes),
            },
        }
    }

    #[test]
    fn an_image_becomes_a_marker_and_its_bytes_come_out_separately() {
        let split = split(
            &req(vec![parts(vec![
                ContentPart::Text {
                    text: "what is this?".into(),
                },
                image(b"PNGDATA"),
            ])]),
            MARKER,
        )
        .unwrap();

        assert_eq!(split.images, vec![b"PNGDATA".to_vec()]);
        let ChatMessage::User {
            content: UserContent::Text(text),
        } = &split.messages[0]
        else {
            panic!("the multipart message did not collapse to text");
        };
        assert_eq!(text, "what is this?\n<__media__>");
    }

    #[test]
    fn two_images_keep_transcript_order_and_are_never_deduplicated() {
        // mtmd pairs the Nth marker with the Nth bitmap. Deduplicating the two
        // identical images would leave two markers and one bitmap, which mtmd
        // rejects; reordering would silently caption the wrong picture.
        let same = b"SAME".to_vec();
        let split = split(
            &req(vec![
                parts(vec![image(b"FIRST"), image(&same)]),
                ChatMessage::assistant("ok"),
                parts(vec![image(&same)]),
            ]),
            MARKER,
        )
        .unwrap();

        assert_eq!(
            split.images,
            vec![b"FIRST".to_vec(), same.clone(), same],
            "images must arrive in transcript order, with duplicates kept",
        );
        assert_eq!(split.images.len(), image_count(&req(vec![])) + 3);
    }

    #[test]
    fn the_marker_count_always_matches_the_image_count() {
        // The invariant mtmd enforces on the other side, asserted here so a
        // rewrite that drops a marker fails in this suite.
        let request = req(vec![
            parts(vec![
                ContentPart::Text { text: "one".into() },
                image(b"A"),
                image(b"B"),
            ]),
            ChatMessage::user("plain text, no parts"),
        ]);
        let split = split(&request, MARKER).unwrap();

        let markers: usize = split
            .messages
            .iter()
            .map(|m| match m {
                ChatMessage::User {
                    content: UserContent::Text(t),
                } => t.matches(MARKER).count(),
                _ => 0,
            })
            .sum();
        assert_eq!(markers, split.images.len());
        assert_eq!(markers, image_count(&request));
    }

    #[test]
    fn a_text_only_transcript_is_unchanged_and_carries_no_images() {
        // The path every ordinary turn takes. `has_images` false is what keeps
        // the text path from being routed through mtmd for no reason.
        let request = req(vec![
            ChatMessage::system("be helpful"),
            ChatMessage::user("hello"),
        ]);
        let split = split(&request, MARKER).unwrap();
        assert!(!split.has_images());
        assert_eq!(image_count(&request), 0);
        assert_eq!(split.messages.len(), 2);
    }

    #[test]
    fn a_remote_image_url_is_refused_rather_than_fetched() {
        // The running core makes no network request the user did not ask for,
        // and fetching an `https:` image here would be one.
        let err = split(
            &req(vec![parts(vec![ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "https://example.com/cat.png".into(),
                },
            }])]),
            MARKER,
        )
        .unwrap_err();
        assert!(
            err.message.contains("will not fetch"),
            "expected a refusal naming the reason, got: {}",
            err.message,
        );
    }

    #[test]
    fn a_malformed_data_uri_is_named_by_index() {
        // A turn can carry several, so "invalid base64" without an index sends
        // the user looking at all of them.
        let err = split(
            &req(vec![parts(vec![
                image(b"fine"),
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "data:image/png;base64,!!!not base64!!!".into(),
                    },
                },
            ])]),
            MARKER,
        )
        .unwrap_err();
        assert!(
            err.message.contains("image 1"),
            "the failure must name which image it was, got: {}",
            err.message,
        );
    }
}
