//! Walking the tree and measuring it.
//!
//! # Symlinks are never followed, and that is load-bearing twice
//!
//! A directory entry is descended only when its own file type is a directory,
//! and stated only when its own file type is a file. `DirEntry::file_type` does
//! not follow links, so a symlink is neither, and the walk skips it entirely:
//! it contributes no bytes and it never becomes a node.
//!
//! Not becoming a node is what keeps it out of the delete index, and the delete
//! index is the whole of the traversal guard (see [`super::safety`]). **So the
//! property that makes the sizes right is the same property that makes the
//! deletes safe.**
//!
//! That matters for whoever eventually decides that following symlinks would
//! report disk usage more truthfully. It would, and it would also put paths
//! outside the core root into the index and make them deletable through the
//! storage screen. If a future version needs to follow links, it needs a
//! separate real-path check on the delete side first, added in the same change.
//!
//! # What a size is
//!
//! Apparent size, never allocated blocks: `Metadata::len()`, the same `st_size`
//! that `Deno.stat().size` reports. A sparse or filesystem-compressed file
//! reports its logical length, so the number answers "how big is this" rather
//! than "how much would be freed". Hard links are not deduplicated, so two
//! directory entries pointing at one inode are counted twice, which is what the
//! oracle does.
//!
//! Every failure is zero. An unreadable directory contributes nothing and is
//! not descended, an unreadable file stats as zero, and a directory entry that
//! vanishes mid-walk is skipped. Nothing here returns an error, so a broken
//! corner of the tree can never blank the storage view.

use std::cmp::Ordering;
use std::path::Path;

/// Extensions that count as model or support files: `gguf` (llama.cpp), `onnx`
/// (Whisper and Kokoro), `txt` (the sherpa token tables and the Kokoro voice
/// list), `bin` (the Kokoro voices pack and GGUF siblings), `json` and `pt`.
pub const MODEL_FILE_EXTS: [&str; 6] = ["gguf", "bin", "onnx", "json", "txt", "pt"];

/// Apparent size of one file, or zero if it cannot be stated.
///
/// Follows symlinks, like `Deno.stat`. The walk only ever calls this for an
/// entry whose own type is already a regular file, so the difference shows up
/// only for a path handed in from outside the walk, such as an installed binary
/// that happens to be a link.
pub fn stat_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// Recursive apparent size of a directory tree.
///
/// Iterative rather than recursive: the depth of a user's models or cache
/// directory is not something this module gets to bound, and an explicit stack
/// cannot overflow on a deep one.
pub fn dir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                total = total.saturating_add(stat_size(&entry.path()));
            }
        }
    }
    total
}

/// The lowercased text after the last dot, or empty when there is none.
///
/// Deliberately not `Path::extension`, which the two cores disagree with:
/// JavaScript's `name.split(".").pop()` yields `gguf` for a file named
/// `.gguf`, while `Path::extension` reads that as a dotfile with no extension
/// and returns `None`. Using it here would silently drop such a file from the
/// models view.
pub fn file_extension(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) => name[dot + 1..].to_lowercase(),
        None => String::new(),
    }
}

/// Whether a file name counts as a model or support file.
pub fn is_model_file(name: &str) -> bool {
    let ext = file_extension(name);
    MODEL_FILE_EXTS.contains(&ext.as_str())
}

/// The ordering the storage view is sorted by, standing in for JavaScript's
/// `localeCompare`.
///
/// `str::cmp` is byte order, which puts every uppercase letter before every
/// lowercase one and visibly reorders a list of HuggingFace repo names against
/// what the oracle produces. ICU root collation compares case-insensitively
/// first and puts lowercase first at the tiebreak, which is what the two levels
/// here reproduce.
///
/// **Deliberately approximate.** It is exact for the ASCII alphanumeric names
/// this actually sorts (repo names, `core.log` and its rotated siblings,
/// extension display names) and can still disagree with ICU on punctuation,
/// which weights `_` before digits where ASCII puts it after. That gap is
/// cosmetic in a view whose reader is scanning for large items, and closing it
/// means taking an ICU collation crate for four sorted lists. Do not.
pub fn by_name(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        // Reversed, so lowercase sorts before uppercase as ICU root does.
        .then_with(|| b.cmp(a))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn dir_size_sums_every_file_at_every_depth() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("a.bin"), &[0; 10]);
        write(&tmp.path().join("sub/b.bin"), &[0; 20]);
        write(&tmp.path().join("sub/deeper/c.bin"), &[0; 30]);
        std::fs::create_dir_all(tmp.path().join("empty")).unwrap();

        assert_eq!(dir_size(tmp.path()), 60);
    }

    #[test]
    fn an_empty_or_missing_tree_is_zero() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(dir_size(tmp.path()), 0);
        assert_eq!(dir_size(&tmp.path().join("nope")), 0);
        assert_eq!(stat_size(&tmp.path().join("nope")), 0);
    }

    /// Directories carry no size of their own, so a deep tree of empty
    /// directories reports nothing.
    #[test]
    fn directories_contribute_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("a/b/c/d")).unwrap();
        assert_eq!(dir_size(tmp.path()), 0);
    }

    /// Two names for one inode are two entries, and the oracle counts both.
    /// Deduplicating would report reclaimable bytes more truthfully and would
    /// disagree with the core this is replacing.
    #[test]
    #[cfg(unix)]
    fn a_hard_link_is_counted_once_per_name() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first.bin");
        write(&first, &[0; 100]);
        std::fs::hard_link(&first, tmp.path().join("second.bin")).unwrap();

        assert_eq!(dir_size(tmp.path()), 200);
    }

    /// The guard the delete path depends on. A symlink is neither descended nor
    /// counted, whether it points at a file or at a directory, and whether the
    /// target is inside the tree or outside it.
    #[test]
    #[cfg(unix)]
    fn a_symlink_is_never_followed_and_never_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        write(&outside.join("big.bin"), &[0; 5000]);
        let inside = tmp.path().join("inside");
        write(&inside.join("real.bin"), &[0; 7]);
        std::os::unix::fs::symlink(outside.join("big.bin"), inside.join("link-to-file")).unwrap();
        std::os::unix::fs::symlink(&outside, inside.join("link-to-dir")).unwrap();

        assert_eq!(
            dir_size(&inside),
            7,
            "a followed link would report 5007 or loop"
        );
    }

    #[test]
    fn the_extension_rule_matches_javascript_rather_than_path_extension() {
        assert!(is_model_file("model.gguf"));
        assert!(is_model_file("model.GGUF"), "compared case insensitively");
        // `Path::extension` reads this as a dotfile and returns None, which
        // would drop the file from the view.
        assert!(is_model_file(".gguf"));
        assert!(is_model_file("tokens.txt"));
        assert!(!is_model_file("README"));
        assert!(!is_model_file("notes.md"));
        // Only the last segment counts, so a double extension is judged on it.
        assert!(!is_model_file("archive.tar.gz"));
        assert!(!is_model_file("trailing."));
        assert_eq!(file_extension("no-dot"), "");
    }

    #[test]
    fn names_sort_case_insensitively_with_lowercase_first() {
        assert_eq!(by_name("apple", "Banana"), Ordering::Less);
        assert_eq!(by_name("Apple", "banana"), Ordering::Less);
        // Byte order would put every capital first and reorder the whole list.
        assert_ne!(
            by_name("apple", "Banana"),
            "apple".cmp("Banana"),
            "a byte-order sort would disagree here, which is the point"
        );
        // Same letters, different case: lowercase leads, as ICU root does.
        assert_eq!(by_name("a", "A"), Ordering::Less);
        assert_eq!(by_name("same", "same"), Ordering::Equal);
    }
}
