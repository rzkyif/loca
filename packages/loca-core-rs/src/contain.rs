//! Lexical path containment: does this path stay under that root.
//!
//! **Orchestrator-owned, and it is a security control rather than a utility.**
//! Three packages had grown their own copy of this pair (D5's memories store,
//! D9's storage walker, D12's sandbox derivation) and a fourth was about to,
//! which is how a containment check drifts: nothing fails when two of them
//! disagree, because each one is only ever tested against its own callers.
//!
//! They HAD drifted. One folded `..` with a bare `PathBuf::pop`, which silently
//! discards a leading `..` on a relative path, so `../x` normalised to `x` and
//! an escaping path came back looking contained. Not reachable from that
//! package's callers, which is exactly why it survived: the bug was real, the
//! consequence was zero, and the next caller would have inherited it for free.
//!
//! Lexical on purpose, never `canonicalize`. The path frequently does not exist
//! yet (a declared write target, a download destination), and resolving
//! symlinks would answer a different question than the one every caller is
//! asking. Where symlinks matter, the caller refuses them outright instead.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// Whether `candidate` stays under `root`, comparing lexically normalised
/// forms.
///
/// Both sides are normalised: comparing a raw candidate against a raw root lets
/// `/a/b/../../etc` pass a `starts_with("/a")` test it should fail.
pub fn is_within(root: &Path, candidate: &Path) -> bool {
    normalize(candidate).starts_with(normalize(root))
}

/// Fold `.` and `..` without touching the filesystem.
///
/// `Path::components` rather than a string split, because it already handles
/// the Windows prefix and the separator cases a naive split gets wrong.
///
/// A leading `..` SURVIVES on a relative path and is dropped on an absolute
/// one, so `/a/../..` is `/` while `../a` stays `../a`. That asymmetry is the
/// whole point: on an absolute path there is nothing above the root to escape
/// to, and on a relative one the `..` is the escape, so dropping it is how a
/// traversal disappears on its way through the check meant to catch it.
pub fn normalize(path: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    let mut absolute = false;
    let mut stack: Vec<&OsStr> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => root.push(prefix.as_os_str()),
            Component::RootDir => {
                root.push(std::path::MAIN_SEPARATOR_STR);
                absolute = true;
            }
            Component::CurDir => {}
            Component::ParentDir => match stack.last() {
                Some(last) if *last != OsStr::new("..") => {
                    stack.pop();
                }
                Some(_) => stack.push(OsStr::new("..")),
                None if !absolute => stack.push(OsStr::new("..")),
                None => {}
            },
            Component::Normal(name) => stack.push(name),
        }
    }
    let mut out = root;
    for name in stack {
        out.push(name);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_traversal_out_of_the_root_is_not_contained() {
        let root = Path::new("/srv/loca/memories");
        assert!(is_within(root, Path::new("/srv/loca/memories/notes.md")));
        assert!(is_within(root, Path::new("/srv/loca/memories/a/../b.md")));
        assert!(!is_within(
            root,
            Path::new("/srv/loca/memories/../../etc/passwd")
        ));
        assert!(!is_within(root, Path::new("/etc/passwd")));
    }

    /// The drift that motivated hoisting this, pinned so it cannot come back.
    ///
    /// Folding `..` with a bare `PathBuf::pop` makes this return `x`, because
    /// popping an empty buffer is a silent no-op. The escape then vanishes
    /// before any containment test sees it.
    #[test]
    fn a_leading_parent_survives_on_a_relative_path() {
        assert_eq!(normalize(Path::new("../x")), PathBuf::from("../x"));
        assert_eq!(normalize(Path::new("../../x")), PathBuf::from("../../x"));
        assert_eq!(normalize(Path::new("a/../../x")), PathBuf::from("../x"));
        assert!(!is_within(Path::new("base"), Path::new("../x")));
    }

    /// The mirror case: on an absolute path there is nothing above the root, so
    /// a `..` at the top is dropped rather than kept.
    #[test]
    fn a_parent_above_the_root_is_dropped_on_an_absolute_path() {
        assert_eq!(normalize(Path::new("/a/../..")), PathBuf::from("/"));
        assert_eq!(normalize(Path::new("/../a")), PathBuf::from("/a"));
    }

    #[test]
    fn a_current_dir_segment_is_folded_away() {
        assert_eq!(normalize(Path::new("/a/./b")), PathBuf::from("/a/b"));
        assert!(is_within(Path::new("/a"), Path::new("/a/./b")));
    }

    /// A prefix match on raw strings, rather than on normalised paths, calls
    /// `/a/bc` a child of `/a/b`. Both sides go through `normalize` and the
    /// comparison is component-wise, so it does not.
    #[test]
    fn a_sibling_sharing_a_name_prefix_is_not_contained() {
        assert!(!is_within(Path::new("/a/b"), Path::new("/a/bc")));
    }
}
