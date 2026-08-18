//! The models category: the one part of the view this package walks itself.
//!
//! The layout on disk is `<models>/<user>/<repo>/...`, mirroring a HuggingFace
//! spec. A repo holding one file shows as a single file node named
//! `user/repo/file`; a repo holding several, or holding a projector sibling,
//! shows as a folder the user can expand and reclaim piecemeal.
//!
//! `models_dir()` is not under the channel root. On a default install it is
//! `~/.loca/models`, shared across channels so multi-gigabyte weights are not
//! downloaded once per channel, while everything else lives under
//! `~/.loca/<channel>/core`. Anything that walked "the root" would miss the
//! largest thing on disk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::contract::api::StorageNode;

use super::node::{file_node, folder_node, lock_reason_mut, node_name};
use super::safety;
use super::walk::{by_name, is_model_file, stat_size};

/// Shown on a model file the current settings need. The user sees it as a
/// tooltip on a row that cannot be selected.
pub const LOCK_REASON: &str = "Required by current settings";

/// One model file, before it becomes a node.
struct ModelFile {
    /// Path relative to the repo directory, with forward slashes on every
    /// platform, because it is displayed rather than opened.
    name: String,
    path: PathBuf,
    size: u64,
}

/// Every model repo under `models_dir`, sorted, with locks applied.
pub fn build_nodes(models_dir: &Path, required: &HashSet<String>) -> Vec<StorageNode> {
    let mut out = Vec::new();
    for user in subdirectories(models_dir) {
        let user_name = file_name(&user);
        for repo in subdirectories(&user) {
            let display = format!("{user_name}/{}", file_name(&repo));
            if let Some(node) = repo_node(&repo, display) {
                out.push(node);
            }
        }
    }
    out.sort_by(|a, b| by_name(node_name(a), node_name(b)));
    for node in &mut out {
        apply_lock(node, required);
    }
    out
}

fn repo_node(repo: &Path, display: String) -> Option<StorageNode> {
    let mut files = collect_files(repo);
    if files.is_empty() {
        return None;
    }
    files.sort_by(|a, b| by_name(&a.name, &b.name));

    // A repo with a multimodal projector always expands, even at one file, so
    // the projector and the weights it belongs to are never the same row.
    let has_projector = files
        .iter()
        .any(|file| base_name(&file.name).to_lowercase().starts_with("mmproj"));

    if has_projector || files.len() > 1 {
        let size = files.iter().map(|file| file.size).sum();
        let children = files
            .into_iter()
            .map(|file| file_node(file.name, file.path, file.size))
            .collect();
        Some(folder_node(display, repo.to_path_buf(), size, children))
    } else {
        let file = files.remove(0);
        Some(file_node(
            format!("{display}/{}", file.name),
            file.path,
            file.size,
        ))
    }
}

/// Every model-shaped file under `repo`, at any depth.
///
/// The extension filter decides what is listed, not what is descended: a repo
/// nesting its weights inside a subdirectory is walked all the way down.
fn collect_files(repo: &Path) -> Vec<ModelFile> {
    let mut out = Vec::new();
    let mut pending = vec![repo.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                pending.push(path);
                continue;
            }
            // Neither a file nor a directory means a symlink, which the walk
            // never follows. See the module docs on `walk`.
            if !file_type.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_model_file(&name) {
                continue;
            }
            let size = stat_size(&path);
            let relative = path
                .strip_prefix(repo)
                .map(display_path)
                .unwrap_or_else(|_| name);
            out.push(ModelFile {
                name: relative,
                path,
                size,
            });
        }
    }
    out
}

/// Annotate a node in place, returning whether it ended up locked. A folder is
/// locked only when every one of its children is.
fn apply_lock(node: &mut StorageNode, required: &HashSet<String>) -> bool {
    match node {
        StorageNode::File { path, .. } => {
            if required.contains(&safety::normalize(path)) {
                *lock_reason_mut(node) = Some(LOCK_REASON.to_string());
                true
            } else {
                false
            }
        }
        StorageNode::Folder { children, .. } => {
            if children.is_empty() {
                return false;
            }
            let mut all_locked = true;
            for child in children.iter_mut() {
                // Deliberately not short-circuiting: every child is annotated
                // even after the first unlocked one, because the client shows
                // a tooltip per row and not just per folder.
                if !apply_lock(child, required) {
                    all_locked = false;
                }
            }
            if all_locked {
                *lock_reason_mut(node) = Some(LOCK_REASON.to_string());
            }
            all_locked
        }
    }
}

fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .map(|entry| entry.path())
        .collect()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// The part after the last forward slash of a display name.
fn base_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// A relative path rendered for display: forward slashes everywhere, so the
/// same repo reads the same on Windows and on Unix.
fn display_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::storage::node::{node_lock_reason, node_size};

    fn write(path: &Path, len: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; len]).unwrap();
    }

    fn nodes(dir: &Path) -> Vec<StorageNode> {
        build_nodes(dir, &HashSet::new())
    }

    #[test]
    fn a_repo_with_one_file_is_a_single_row() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("unsloth/Qwen/model.gguf"), 12);

        let out = nodes(tmp.path());
        assert_eq!(out.len(), 1);
        let StorageNode::File { name, size, .. } = &out[0] else {
            panic!("expected a file node, got {:?}", out[0]);
        };
        assert_eq!(name, "unsloth/Qwen/model.gguf");
        assert_eq!(*size, 12);
    }

    #[test]
    fn a_repo_with_several_files_becomes_a_folder_summing_them() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("u/r/a.gguf"), 10);
        write(&tmp.path().join("u/r/b.gguf"), 20);

        let out = nodes(tmp.path());
        let StorageNode::Folder {
            name,
            size,
            children,
            ..
        } = &out[0]
        else {
            panic!("expected a folder node");
        };
        assert_eq!(name, "u/r");
        assert_eq!(*size, 30);
        assert_eq!(children.len(), 2);
    }

    /// One file plus a projector is still two files, so the interesting case is
    /// the projector alone: it expands where any other single file would not.
    #[test]
    fn a_projector_forces_a_folder_even_alone() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("u/r/mmproj-F16.gguf"), 5);

        let out = nodes(tmp.path());
        assert!(
            matches!(out[0], StorageNode::Folder { .. }),
            "a projector always expands, so it never shares a row with weights"
        );
    }

    #[test]
    fn non_model_files_are_skipped_and_an_empty_repo_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("u/r/README.md"), 100);
        write(&tmp.path().join("u/other/model.gguf"), 3);

        let out = nodes(tmp.path());
        assert_eq!(out.len(), 1, "the repo with only a README is not listed");
        assert_eq!(node_size(&out[0]), 3);
    }

    #[test]
    fn nested_files_keep_their_relative_name() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("u/r/split/part-1.gguf"), 1);
        write(&tmp.path().join("u/r/split/part-2.gguf"), 2);

        let out = nodes(tmp.path());
        let StorageNode::Folder { children, .. } = &out[0] else {
            panic!("expected a folder node");
        };
        let names: Vec<_> = children.iter().map(node_name).collect();
        assert_eq!(names, ["split/part-1.gguf", "split/part-2.gguf"]);
    }

    #[test]
    fn repos_and_their_files_sort_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("zeta/repo/m.gguf"), 1);
        write(&tmp.path().join("Alpha/repo/m.gguf"), 1);

        let out = nodes(tmp.path());
        let names: Vec<_> = out.iter().map(node_name).collect();
        assert_eq!(
            names,
            ["Alpha/repo/m.gguf", "zeta/repo/m.gguf"],
            "byte order would put Alpha first for the wrong reason and zeta last"
        );
    }

    #[test]
    fn a_required_file_is_locked_and_its_neighbour_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        let required = tmp.path().join("u/r/needed.gguf");
        write(&required, 1);
        write(&tmp.path().join("u/r/spare.gguf"), 1);

        let locks = HashSet::from([safety::normalize(&required.to_string_lossy())]);
        let out = build_nodes(tmp.path(), &locks);
        let StorageNode::Folder {
            children,
            lock_reason,
            ..
        } = &out[0]
        else {
            panic!("expected a folder node");
        };
        assert_eq!(
            *lock_reason, None,
            "one locked child does not lock the folder"
        );
        let locked: Vec<_> = children
            .iter()
            .map(|child| node_lock_reason(child).is_some())
            .collect();
        assert_eq!(
            locked,
            [true, false],
            "every child is annotated, not just up to the first unlocked one"
        );
    }

    #[test]
    fn a_folder_is_locked_only_when_every_child_is() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("u/r/a.gguf");
        let second = tmp.path().join("u/r/b.gguf");
        write(&first, 1);
        write(&second, 1);

        let locks = HashSet::from([
            safety::normalize(&first.to_string_lossy()),
            safety::normalize(&second.to_string_lossy()),
        ]);
        let out = build_nodes(tmp.path(), &locks);
        assert_eq!(node_lock_reason(&out[0]), Some(LOCK_REASON));
    }

    #[test]
    #[cfg(unix)]
    fn a_symlinked_model_never_becomes_a_node() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside/huge.gguf");
        write(&outside, 9000);
        let repo = tmp.path().join("models/u/r");
        write(&repo.join("real.gguf"), 4);
        std::os::unix::fs::symlink(&outside, repo.join("linked.gguf")).unwrap();

        let out = nodes(&tmp.path().join("models"));
        assert_eq!(out.len(), 1);
        assert_eq!(
            node_size(&out[0]),
            4,
            "the link is neither listed nor counted, so it can never be deleted through the view"
        );
    }
}
