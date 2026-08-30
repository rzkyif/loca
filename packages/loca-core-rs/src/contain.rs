//! Lexical path containment: does this path stay under that root. One copy,
//! because a containment check that exists three times drifts silently.
//!
//! Lexical on purpose, never `canonicalize`: the path frequently does not exist
//! yet (a declared write target, a download destination). Where symlinks
//! matter, the caller refuses them outright instead.

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
/// A leading `..` SURVIVES on a relative path and is dropped on an absolute
/// one, so `/a/../..` is `/` while `../a` stays `../a`: on an absolute path
/// there is nothing above the root to escape to, and on a relative one
/// dropping the `..` would hide the traversal from this very check.
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

    #[test]
    fn a_leading_parent_survives_on_a_relative_path() {
        assert_eq!(normalize(Path::new("../x")), PathBuf::from("../x"));
        assert_eq!(normalize(Path::new("../../x")), PathBuf::from("../../x"));
        assert_eq!(normalize(Path::new("a/../../x")), PathBuf::from("../x"));
        assert!(!is_within(Path::new("base"), Path::new("../x")));
    }

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

    #[test]
    fn a_sibling_sharing_a_name_prefix_is_not_contained() {
        assert!(!is_within(Path::new("/a/b"), Path::new("/a/bc")));
    }
}
