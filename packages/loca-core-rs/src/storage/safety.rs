//! The three primitives every reclaim operation is built from: lexical
//! normalisation, the under-root assertion, and the recursive remove.
//!
//! # The guard that makes traversal impossible rather than filtered
//!
//! **A caller's path string is only ever a lookup key. It is never a delete
//! target.**
//!
//! [`super::delete_paths`] builds the whole storage tree first, indexes it by
//! normalised path, and looks each requested target up in that index. A target
//! that is not in the index is skipped, which is what makes the operation
//! idempotent. A target that IS in the index yields the node Core itself
//! enumerated, and the removal then runs against that node's path.
//!
//! So `..`, an absolute re-root, a UNC path, a NUL byte and a Windows short
//! name are not rejected by a filter: they simply fail to match a key. Deleting
//! the requested path directly, and skipping the index, looks like a
//! simplification and is the worst bug available in this core. Do not do it.
//!
//! [`assert_under`] is the second belt, applied by the categories whose nodes
//! are files on disk. It is not what makes the module safe on its own, and a
//! test that only exercises the pair cannot tell which of them is holding.

use std::path::{Path, PathBuf};

use crate::error::{validation, Result};

/// Lexical normalisation of a path string, matching `@std/path`'s `normalize`,
/// which is what produces the keys the delete index is built on.
///
/// Purely lexical: `..` is resolved against the preceding component without
/// consulting the filesystem, so this says nothing about where a symlink
/// actually points. That is fine here and nowhere else, because the result is
/// only ever compared against keys Core produced itself.
///
/// A trailing separator is preserved, so `<dir>/` and `<dir>` are different keys
/// and the first one matches nothing. Widening that would turn a request
/// answered with "skipped" into a delete.
pub fn normalize(path: &str) -> String {
    let resolved = normalize_path(Path::new(path));
    let mut out = resolved.to_string_lossy().into_owned();
    if out.is_empty() {
        out.push('.');
    }
    // `normalize("/a/b/") == "/a/b/"` but `normalize("/a/../") == "/"`: the
    // separator is re-appended only when the result does not already end in
    // one, which is exactly the root case.
    if ends_with_separator(path) && !out.ends_with(std::path::MAIN_SEPARATOR) {
        out.push(std::path::MAIN_SEPARATOR);
    }
    out
}

/// [`normalize`] over a [`Path`], without the trailing-separator rule that only
/// matters for index keys.
///
/// Delegates to [`crate::contain::normalize`], which is the crate's one copy of
/// the `..`-folding logic. This module carried a byte-identical second one, in
/// exactly the shape `contain.rs` was created to end: its header records that
/// three packages had each grown a copy, that two had already drifted, and that
/// "nothing fails when two of them disagree, because each one is only ever
/// tested against its own callers". This was the copy backing `assert_under` on
/// the destructive reclaim paths, so it was the one whose drift would have cost
/// the most.
///
/// What stays local is what genuinely differs: the trailing-separator rule
/// above, which only index keys care about, and `assert_under`'s
/// strict-descendant semantics below.
pub fn normalize_path(path: &Path) -> PathBuf {
    crate::contain::normalize(path)
}

fn ends_with_separator(path: &str) -> bool {
    path.ends_with('/') || (cfg!(windows) && path.ends_with('\\'))
}

/// Reject any path that does not sit strictly inside `root`: no `..` escape, no
/// absolute re-root, and not the root directory itself.
///
/// Both sides are normalised first, so `<root>/a/../b` is accepted (it really
/// is under the root) while `<root>/../b` is not. Component-wise comparison is
/// what stops a shared textual prefix from passing: `<root>-2/x` is not under
/// `<root>`.
pub fn assert_under(target: &Path, root: &Path) -> Result<()> {
    let target = normalize_path(target);
    let root = normalize_path(root);
    match target.strip_prefix(&root) {
        // An empty remainder means the target IS the root. Removing the models
        // directory, the bin directory or the logs directory wholesale is not
        // something any node in the tree asks for.
        Ok(rest) if !rest.as_os_str().is_empty() => Ok(()),
        _ => Err(validation(format!(
            "path is not under {}: {}",
            root.display(),
            target.display()
        ))),
    }
}

/// Remove a file or a directory tree. A path that is already gone is success,
/// which is what makes a repeated delete idempotent; every other error
/// propagates.
///
/// The kind is read with `symlink_metadata`, so a symlink is unlinked rather
/// than followed. `remove_dir_all` on a symlink to a directory would fail with
/// "not a directory", where `Deno.remove` removes the link. Nothing in the
/// index is ever a symlink (see [`super::walk`]), so this is the second line
/// rather than the first.
pub fn remove_path(path: &Path) -> Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let removed = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The table is the recorded output of `@std/path`'s `normalize`, produced
    /// by running it rather than transcribed from its documentation. The
    /// trailing-separator rule is the half that is easy to get wrong and the
    /// half that changes whether a delete happens.
    #[test]
    #[cfg(unix)]
    fn normalize_matches_the_recorded_typescript_output() {
        for (input, want) in [
            ("", "."),
            (".", "."),
            ("/a/b", "/a/b"),
            ("/a/b/", "/a/b/"),
            ("/a//b", "/a/b"),
            ("/a/./b", "/a/b"),
            ("/a/b/../b", "/a/b"),
            ("/a/b/..", "/a"),
            ("/a/../..", "/"),
            ("/a/b/../", "/a/"),
            ("/a/../", "/"),
            ("//", "/"),
            ("///a", "/a"),
            ("/", "/"),
            ("..", ".."),
            ("../a", "../a"),
            ("a/b/", "a/b/"),
            ("a/..", "."),
            ("a/../", "./"),
            ("./a", "a"),
            ("/a/b/./", "/a/b/"),
        ] {
            assert_eq!(normalize(input), want, "normalize({input:?})");
        }
    }

    #[test]
    fn assert_under_accepts_only_a_strict_descendant() {
        let root = Path::new("/root");
        assert!(assert_under(Path::new("/root/a"), root).is_ok());
        assert!(assert_under(Path::new("/root/a/b/c"), root).is_ok());
        // Really under the root, spelled with a detour.
        assert!(assert_under(Path::new("/root/a/../b"), root).is_ok());
    }

    #[test]
    fn assert_under_rejects_the_root_itself_and_every_escape() {
        let root = Path::new("/root");
        for target in [
            "/root",           // the root itself
            "/root/",          // the root, spelled with a separator
            "/root/a/../..",   // climbs back out
            "/root/../secret", // classic traversal
            "/other",          // unrelated absolute path
            "/rootx/a",        // shares a textual prefix, is not a descendant
            "relative/path",   // not absolute at all
        ] {
            assert!(
                assert_under(Path::new(target), root).is_err(),
                "expected {target} to be refused"
            );
        }
    }

    #[test]
    fn remove_path_is_idempotent_and_recursive() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tree/nested");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), b"x").unwrap();

        remove_path(&tmp.path().join("tree")).unwrap();
        assert!(!tmp.path().join("tree").exists());
        // Already gone is success, not an error.
        remove_path(&tmp.path().join("tree")).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn remove_path_unlinks_a_symlink_without_touching_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep.txt"), b"keep").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        remove_path(&link).unwrap();
        assert!(!link.exists(), "the link should be gone");
        assert!(
            target.join("keep.txt").exists(),
            "the link's target must survive"
        );
    }
}
