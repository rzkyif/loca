//! Listing, deleting and locating model files on disk.
//!
//! The layout under the models root mirrors a HuggingFace spec with the branch
//! dropped: `@user/repo/branch/file` lives at `<models>/user/repo/file`, so the
//! same file is not tracked once per branch.

use std::path::{Path, PathBuf};

use crate::contract::api::ModelEntry;
use crate::error::{validation, AppError, ErrorCode, Result};

/// `localeCompare` for a listing, approximating ICU root collation: case
/// insensitive, with lowercase ahead of uppercase on an otherwise equal
/// comparison.
///
/// A copy of `storage::walk::by_name`, because `storage::walk` is private to
/// D9's module and this package cannot widen it. The two must agree: they order
/// two views of the same directory, and a user comparing the model list with
/// the storage screen would otherwise see the same files in two orders. There
/// is an open request to hoist it; when that lands this goes away.
fn by_name(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| b.cmp(a))
}

/// Resolve an HF spec to its absolute path under the models root.
///
/// A spec that does not start with `@` is already an absolute path and is
/// returned unchanged, which is how a hand-typed local model still works. A
/// spec that starts with `@` but does not have four segments is malformed and
/// is also returned as-is: this is a resolver, not a validator, and the caller
/// that tries to open it produces the better error.
///
/// Shared with the sidecars (D14, N2, N3) and with D9's storage locks, so it is
/// the one place the branch-dropping rule lives.
pub fn resolve_hf_path(models_dir: &Path, spec: &str) -> PathBuf {
    let Some(rest) = spec.strip_prefix('@') else {
        return PathBuf::from(spec);
    };
    let mut parts = rest.splitn(4, '/');
    let (Some(user), Some(repo), Some(_branch), Some(file)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return PathBuf::from(spec);
    };
    if user.is_empty() || repo.is_empty() || file.is_empty() {
        return PathBuf::from(spec);
    }
    let mut path = models_dir.join(user).join(repo);
    // The file part may itself be a nested repo path.
    for segment in file.split('/') {
        path.push(segment);
    }
    path
}

/// Reject a relative path that would escape the models root.
fn sanitize_rel_path(rel_path: &str) -> Result<&str> {
    if rel_path.contains('\0') || rel_path.contains("..") {
        return Err(validation(format!("invalid model relPath: {rel_path}")));
    }
    Ok(rel_path.trim_start_matches('/'))
}

/// Delete one model file. A file that is not there is `model_not_found` (404)
/// rather than a silent success, because the client shows the row it just tried
/// to remove.
pub async fn delete(models_dir: &Path, rel_path: &str) -> Result<()> {
    let safe = sanitize_rel_path(rel_path)?;
    let absolute = models_dir.join(safe);
    match tokio::fs::remove_file(&absolute).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(AppError::new(
            ErrorCode::ModelNotFound,
            format!("model not on disk: {rel_path}"),
        )),
        Err(err) => Err(err.into()),
    }
}

/// Every file under the models root, flat, sorted by relative path.
///
/// A missing root is an empty list, not an error: a core that has never
/// downloaded a model has no models directory.
pub async fn list(models_dir: &Path) -> Result<Vec<ModelEntry>> {
    let root = models_dir.to_path_buf();
    let mut out = Vec::new();
    walk(&root, &root, &mut out).await;
    // The same comparator the storage view uses, so two views of one directory
    // cannot disagree about order.
    out.sort_by(|a, b| by_name(&a.rel_path, &b.rel_path));
    Ok(out)
}

/// Recursive, and iterative rather than recursive in Rust terms because an
/// `async fn` cannot call itself without boxing.
async fn walk(root: &Path, start: &Path, out: &mut Vec<ModelEntry>) {
    let mut stack = vec![start.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            // A directory that vanished or cannot be read is skipped, matching
            // the TypeScript's NotFound branch. The root not existing is the
            // common case and is not an error.
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let full = entry.path();
            let Ok(kind) = entry.file_type().await else {
                continue;
            };
            if kind.is_dir() {
                stack.push(full);
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata().await else {
                continue; // unreadable: skipped, never fatal
            };
            let Ok(relative) = full.strip_prefix(root) else {
                continue;
            };
            let rel_path = relative.to_string_lossy().replace('\\', "/");
            out.push(ModelEntry {
                source: source_for(&rel_path),
                rel_path,
                abs_path: full.to_string_lossy().into_owned(),
                size_bytes: meta.len() as i64,
            });
        }
    }
}

/// Reconstruct the HF spec a relative path came from, re-inserting `main` as
/// the branch. The branch is not recoverable from disk by design, and `main` is
/// what the download path uses.
fn source_for(rel_path: &str) -> String {
    let mut parts = rel_path.splitn(3, '/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(user), Some(repo), Some(file)) if !user.is_empty() && !repo.is_empty() => {
            format!("@{user}/{repo}/main/{file}")
        }
        // Fewer than three segments is not a repo layout, so there is no branch
        // to re-insert and the path is passed through as the TypeScript's regex
        // does when it does not match.
        _ => format!("@{rel_path}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn an_hf_spec_resolves_with_its_branch_dropped() {
        let root = Path::new("/models");
        assert_eq!(
            resolve_hf_path(root, "@unsloth/Qwen3.5-4B-GGUF/main/Qwen3.5-4B-Q4_K_M.gguf"),
            root.join("unsloth")
                .join("Qwen3.5-4B-GGUF")
                .join("Qwen3.5-4B-Q4_K_M.gguf")
        );
        // A different branch lands on the SAME path, which is the whole point:
        // the same bytes are not stored once per branch.
        assert_eq!(
            resolve_hf_path(root, "@u/r/main/f.gguf"),
            resolve_hf_path(root, "@u/r/experimental/f.gguf")
        );
        // A nested repo path keeps its structure.
        assert_eq!(
            resolve_hf_path(root, "@u/r/main/sub/dir/f.gguf"),
            root.join("u")
                .join("r")
                .join("sub")
                .join("dir")
                .join("f.gguf")
        );
    }

    #[test]
    fn a_non_hf_spec_is_already_a_path() {
        let root = Path::new("/models");
        assert_eq!(
            resolve_hf_path(root, "/opt/models/local.gguf"),
            PathBuf::from("/opt/models/local.gguf")
        );
        // Malformed `@` specs pass through rather than resolving somewhere
        // surprising under the root.
        assert_eq!(resolve_hf_path(root, "@u/r"), PathBuf::from("@u/r"));
        assert_eq!(resolve_hf_path(root, "@"), PathBuf::from("@"));
        assert_eq!(
            resolve_hf_path(root, "@//main/f"),
            PathBuf::from("@//main/f")
        );
    }

    #[test]
    fn a_traversing_rel_path_is_refused_before_it_is_joined() {
        for bad in [
            "../outside.gguf",
            "u/../../outside.gguf",
            "u/r/..",
            "u/r/\0.gguf",
        ] {
            let err = sanitize_rel_path(bad).expect_err(bad);
            assert_eq!(err.code, ErrorCode::ValidationError, "{bad}");
        }
        // A leading slash is stripped rather than refused, so an absolute-looking
        // path from the client still resolves under the root.
        assert_eq!(sanitize_rel_path("/u/r/f.gguf").unwrap(), "u/r/f.gguf");
        assert_eq!(sanitize_rel_path("u/r/f.gguf").unwrap(), "u/r/f.gguf");
    }

    #[tokio::test]
    async fn listing_walks_every_depth_and_reconstructs_the_spec() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "unsloth/Repo-GGUF/model.gguf", b"aaaa");
        write(root, "unsloth/Repo-GGUF/nested/extra.gguf", b"bb");
        write(root, "another/Repo/mmproj-F16.gguf", b"c");

        let entries = list(root).await.unwrap();
        assert_eq!(entries.len(), 3);
        let by_rel: std::collections::HashMap<&str, &ModelEntry> =
            entries.iter().map(|e| (e.rel_path.as_str(), e)).collect();

        let one = by_rel["unsloth/Repo-GGUF/model.gguf"];
        assert_eq!(one.source, "@unsloth/Repo-GGUF/main/model.gguf");
        assert_eq!(one.size_bytes, 4);
        assert_eq!(
            one.abs_path,
            root.join("unsloth/Repo-GGUF/model.gguf").to_string_lossy()
        );

        // A nested file keeps the rest of its path in the spec's file segment.
        assert_eq!(
            by_rel["unsloth/Repo-GGUF/nested/extra.gguf"].source,
            "@unsloth/Repo-GGUF/main/nested/extra.gguf"
        );
        assert_eq!(by_rel["another/Repo/mmproj-F16.gguf"].size_bytes, 1);
    }

    #[tokio::test]
    async fn listing_sorts_by_relative_path_and_tolerates_a_missing_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Names that differ by LETTER as well as case: a macOS or Windows
        // filesystem is case-insensitive, so two entries differing only in case
        // are one entry and the fixture would silently test nothing.
        for rel in ["gamma/r/m.gguf", "alpha/r/m.gguf", "Beta/r/m.gguf"] {
            write(root, rel, b"x");
        }
        let entries = list(root).await.unwrap();
        let order: Vec<&str> = entries.iter().map(|e| e.rel_path.as_str()).collect();
        // Case-insensitive, which `localeCompare` is and a byte-wise sort is
        // not: the latter would put "Beta" first.
        assert_eq!(
            order,
            vec!["alpha/r/m.gguf", "Beta/r/m.gguf", "gamma/r/m.gguf"]
        );

        // A core that has never downloaded anything has no models directory,
        // and that is an empty list rather than a 500.
        assert!(list(&root.join("does-not-exist")).await.unwrap().is_empty());
    }

    /// The comparator itself, where a case collision on disk cannot get in the
    /// way. It has to agree with `storage::walk::by_name`, which orders the
    /// same files on the storage screen.
    #[test]
    fn the_comparator_is_case_insensitive_then_lowercase_first() {
        use std::cmp::Ordering;
        assert_eq!(by_name("alpha", "Beta"), Ordering::Less);
        assert_eq!(by_name("Beta", "gamma"), Ordering::Less);
        // Equal ignoring case: lowercase wins, as ICU root collation does and
        // as a byte-wise comparison does not.
        assert_eq!(by_name("beta", "Beta"), Ordering::Less);
        assert_eq!(by_name("Beta", "beta"), Ordering::Greater);
        assert_eq!(by_name("same", "same"), Ordering::Equal);
    }

    #[tokio::test]
    async fn deleting_a_missing_file_is_model_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "u/r/f.gguf", b"data");

        delete(root, "u/r/f.gguf").await.unwrap();
        assert!(!root.join("u/r/f.gguf").exists());

        let err = delete(root, "u/r/f.gguf").await.expect_err("gone now");
        assert_eq!(err.code, ErrorCode::ModelNotFound);

        let err = delete(root, "../escape").await.expect_err("refused");
        assert_eq!(err.code, ErrorCode::ValidationError);
    }
}
