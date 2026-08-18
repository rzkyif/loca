//! Attachment handling: the filename sanitiser the upload route runs, and the
//! resolution the prompt builder runs.
//!
//! Resolution reads an attachment's bytes back off disk given the URL the client
//! stored on a message part. It lives here, with the store that records the
//! attachment, rather than with the prompt builder that consumes it: assembling
//! provider messages needs the LLM package's request shape and the memories
//! package's token expansion, and neither belongs in a session store.

use std::collections::HashMap;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

use super::store::SessionsStore;

// --- filenames ---------------------------------------------------------------

/// Windows refuses these basenames even with an extension.
const WIN_RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

const MAX_FILENAME_LEN: usize = 200;
const MAX_EXTENSION_LEN: usize = 16;

/// Make an uploaded filename safe to join as a single path segment.
///
/// Filenames here are attacker-controlled and end up on disk as
/// `<attachmentId>_<filename>`. The id prefix already blocks traversal on its
/// own, but `/` and `\` are stripped anyway, Windows has rules a simple replace
/// does not cover (reserved basenames, trailing dots and spaces, length limits),
/// and control characters and shell glyphs are worth removing on every host.
/// Always returns a non-empty string.
pub fn sanitize_filename(name: &str) -> String {
    // 1. Strip control characters, and replace the path, shell and
    //    Windows-forbidden glyphs.
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();

    // 2. Strip leading dots (hidden files) and trailing dots and spaces, which
    //    Windows silently drops, colliding the name with its neighbours.
    out = out
        .trim_start_matches('.')
        .trim_end_matches(['.', ' '])
        .to_string();

    // 3. Cap the length, keeping a short extension.
    if out.chars().count() > MAX_FILENAME_LEN {
        let ext_start = out
            .rfind('.')
            .filter(|dot| *dot > 0 && out[*dot..].chars().count() <= MAX_EXTENSION_LEN);
        out = match ext_start {
            Some(dot) => {
                let ext = out[dot..].to_string();
                let keep = MAX_FILENAME_LEN.saturating_sub(ext.chars().count());
                let head: String = out.chars().take(keep).collect();
                format!("{head}{ext}")
            }
            None => out.chars().take(MAX_FILENAME_LEN).collect(),
        };
    }

    // 4. Refuse a Windows reserved basename, prefixed rather than replaced so
    //    the file stays recognizable.
    let stem = out
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if WIN_RESERVED.contains(&stem.as_str()) {
        out = format!("_{out}");
    }

    // 5. Everything stripped. The caller still prepends an id, so this is only
    //    informational.
    if out.is_empty() {
        out = "file".to_string();
    }
    out
}

// --- resolution --------------------------------------------------------------

/// Per-request memoization of resolved attachments, so one attachment referenced
/// by several turns is read once. A cached `None` is a remembered failure, which
/// is the point: a missing file should not be re-read for every turn.
pub type AttachmentCache = HashMap<String, Option<String>>;

/// The trailing attachment id in the URL a client stored on a message part
/// (`<baseUrl>/api/v1/sessions/<sessionId>/attachments/<attachmentId>`).
///
/// `None` when the path does not match, and the caller skips that attachment
/// rather than failing the whole turn.
pub fn attachment_id_from_path(path: &str) -> Option<&str> {
    let id = path.rsplit_once("/attachments/")?.1;
    let id = id.split(['?', '#']).next()?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

/// An image attachment as a `data:` URI, ready to inline in a provider request.
///
/// Every failure is a warning and a skipped attachment. One unreadable image
/// must not fail a turn that would otherwise have answered.
pub async fn read_attachment_as_data_url(
    store: &SessionsStore,
    session_id: &str,
    path: &str,
    fallback_mime: &str,
    cache: Option<&mut AttachmentCache>,
) -> Option<String> {
    resolve(store, session_id, path, "img", cache, |record, bytes| {
        let mime = record.mime.as_deref().unwrap_or(fallback_mime);
        Some(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
    })
    .await
}

/// A document attachment as text. The client converts every non-image
/// attachment to markdown before uploading, so the bytes are already text.
pub async fn read_attachment_as_text(
    store: &SessionsStore,
    session_id: &str,
    path: &str,
    cache: Option<&mut AttachmentCache>,
) -> Option<String> {
    resolve(store, session_id, path, "doc", cache, |_record, bytes| {
        String::from_utf8(bytes).ok()
    })
    .await
}

async fn resolve(
    store: &SessionsStore,
    session_id: &str,
    path: &str,
    cache_prefix: &str,
    cache: Option<&mut AttachmentCache>,
    render: impl FnOnce(&super::doc::AttachmentRecord, Vec<u8>) -> Option<String>,
) -> Option<String> {
    let id = attachment_id_from_path(path)?;
    let key = format!("{cache_prefix}:{id}");
    if let Some(cache) = cache.as_deref() {
        if let Some(hit) = cache.get(&key) {
            return hit.clone();
        }
    }
    let result = match store.get_attachment(session_id, id).await {
        Ok(record) => match super::store::read_file(record.abs_path.clone().into()).await {
            Ok(bytes) => render(&record, bytes),
            Err(err) => {
                tracing::warn!(
                    target: "loca::chat.attachments",
                    "attachment load failed ({path}): {}", err.message,
                );
                None
            }
        },
        Err(err) => {
            tracing::warn!(
                target: "loca::chat.attachments",
                "attachment lookup failed ({path}): {}", err.message,
            );
            None
        }
    };
    if let Some(cache) = cache {
        cache.insert(key, result.clone());
    }
    result
}
