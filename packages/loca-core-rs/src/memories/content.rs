//! The on-disk half of a memory: paths, slugs, titles, and the filesystem
//! calls. Knowledge is `<slug>.md`; a skill is `<slug>/SKILL.md` plus bundled
//! files. Both sit under their provider's base directory.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{internal, AppError, Result};

use crate::contract::api::MemoryKind;

/// The instruction body inside a skill folder. The folder name is the slug.
pub const SKILL_FILE: &str = "SKILL.md";

/// sha256 of the exact bytes on disk, lowercase hex. What every freshness
/// comparison pins to.
pub fn content_hash(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

/// Where a memory's content lives, given its provider's base directory.
pub fn memory_path(base: &Path, kind: MemoryKind, filename: &str) -> PathBuf {
    match kind {
        MemoryKind::Skill => base.join(filename).join(SKILL_FILE),
        MemoryKind::Knowledge => base.join(filename),
    }
}

/// `"Meeting Notes: Q3!"` -> `"meeting-notes-q3"`: lowercase, runs outside
/// ASCII `[a-z0-9]` collapsed to one `-`, edges trimmed, then cut to 64 (so a
/// slug may still end in `-`).
pub fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut pending_dash = false;
    for c in title.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c);
        } else {
            pending_dash = true;
        }
    }
    out.truncate(64);
    if out.is_empty() {
        "memory".to_string()
    } else {
        out
    }
}

/// `"skills/file-bug"` -> `"File Bug"`, `"knowledge/release-notes.md"` ->
/// `"Release Notes"`. The title an extension's declared memory gets.
pub fn pretty_title(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let without_ext = match last.len().checked_sub(3) {
        Some(cut) if last[cut..].eq_ignore_ascii_case(".md") => &last[..cut],
        _ => last,
    };
    let spaced: String = without_ext
        .chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect();
    let words: Vec<String> = spaced
        .split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        "Memory".to_string()
    } else {
        words.join(" ")
    }
}

/// Traversal guard for bundled files: a plain name directly inside the skill
/// folder. `SKILL.md` is refused case-insensitively so a case-insensitive
/// filesystem cannot route a bundled `skill.md` onto the instruction body.
pub fn check_skill_file_name(name: &str) -> Result<()> {
    if name.trim().is_empty()
        || name.eq_ignore_ascii_case(SKILL_FILE)
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
    {
        return Err(crate::error::validation(format!(
            "invalid skill file \"{name}\""
        )));
    }
    Ok(())
}

// --- filesystem: `spawn_blocking` over `std::fs`, one hop per operation ------

async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> std::io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| internal(format!("filesystem task failed: {err}")))?
        .map_err(AppError::from)
}

pub async fn read_text(path: PathBuf) -> Result<String> {
    blocking(move || std::fs::read_to_string(path)).await
}

pub async fn create_dir_all(path: PathBuf) -> Result<()> {
    blocking(move || std::fs::create_dir_all(path)).await
}

/// True when the path exists at all, directory or file.
pub async fn exists(path: PathBuf) -> bool {
    tokio::task::spawn_blocking(move || path.exists())
        .await
        .unwrap_or(false)
}

/// Remove a file, or a directory recursively. A missing path is not an error.
pub async fn remove(path: PathBuf, recursive: bool) -> Result<()> {
    blocking(move || {
        let result = if recursive {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    })
    .await
}

/// One directory entry: its name and whether it is a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Immediate entries of a directory in filesystem order (not sorted: `rescan`
/// adoption order is observable). Empty when the directory is absent.
pub async fn read_dir(path: PathBuf) -> Vec<DirEntry> {
    tokio::task::spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|e| DirEntry {
                name: e.file_name().to_string_lossy().into_owned(),
                is_dir: e.file_type().is_ok_and(|t| t.is_dir()),
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Write a memory's content atomically, creating a skill's folder first.
pub async fn write_memory_content(
    base: &Path,
    kind: MemoryKind,
    filename: &str,
    content: &str,
) -> Result<()> {
    if kind == MemoryKind::Skill {
        create_dir_all(base.join(filename)).await?;
    }
    crate::files::write_atomic(
        memory_path(base, kind, filename),
        content.as_bytes().to_vec(),
    )
    .await
}
