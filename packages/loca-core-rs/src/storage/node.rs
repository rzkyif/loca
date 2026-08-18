//! Constructing and reading a [`StorageNode`].
//!
//! The wire type is a tagged enum with two struct variants that share four of
//! their five fields, so every read of a shared field is a two-arm match. These
//! helpers keep that match in one place instead of at forty call sites.
//!
//! `size` is `f64` on the wire, because the TypeScript declares it `number`.
//! Sizes are accumulated as `u64` and converted once, here, so a large tree
//! cannot lose precision part way through a sum.

use std::path::PathBuf;

use crate::contract::api::StorageNode;

pub fn file_node(name: String, path: PathBuf, size: u64) -> StorageNode {
    StorageNode::File {
        name,
        path: path.to_string_lossy().into_owned(),
        size: size as i64,
        lock_reason: None,
    }
}

pub fn folder_node(
    name: String,
    path: PathBuf,
    size: u64,
    children: Vec<StorageNode>,
) -> StorageNode {
    StorageNode::Folder {
        name,
        path: path.to_string_lossy().into_owned(),
        size: size as i64,
        children,
        lock_reason: None,
    }
}

/// A folder with no children, for a category whose rows are opaque to the view
/// (a session, an installed extension, a cache directory).
pub fn leaf_folder(name: String, path: PathBuf, size: u64) -> StorageNode {
    folder_node(name, path, size, Vec::new())
}

pub fn node_name(node: &StorageNode) -> &str {
    match node {
        StorageNode::File { name, .. } | StorageNode::Folder { name, .. } => name,
    }
}

pub fn node_path(node: &StorageNode) -> &str {
    match node {
        StorageNode::File { path, .. } | StorageNode::Folder { path, .. } => path,
    }
}

pub fn node_size(node: &StorageNode) -> i64 {
    match node {
        StorageNode::File { size, .. } | StorageNode::Folder { size, .. } => *size,
    }
}

pub fn node_lock_reason(node: &StorageNode) -> Option<&str> {
    match node {
        StorageNode::File { lock_reason, .. } | StorageNode::Folder { lock_reason, .. } => {
            lock_reason.as_deref()
        }
    }
}

pub fn lock_reason_mut(node: &mut StorageNode) -> &mut Option<String> {
    match node {
        StorageNode::File { lock_reason, .. } | StorageNode::Folder { lock_reason, .. } => {
            lock_reason
        }
    }
}

/// Set a lock reason, or leave the node alone when there is none.
pub fn with_lock(mut node: StorageNode, reason: Option<String>) -> StorageNode {
    *lock_reason_mut(&mut node) = reason;
    node
}

/// The reason this node or any node beneath it is in use, which is what the
/// delete path refuses on. A folder is undeletable while any single descendant
/// is locked, even when the folder itself is not.
pub fn subtree_lock_reason(node: &StorageNode) -> Option<&str> {
    if let Some(reason) = node_lock_reason(node) {
        return Some(reason);
    }
    let StorageNode::Folder { children, .. } = node else {
        return None;
    };
    children.iter().find_map(subtree_lock_reason)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_locked_descendant_locks_the_whole_subtree() {
        let locked = with_lock(
            file_node("child".into(), "/tmp/child".into(), 1),
            Some("Session is active".into()),
        );
        let folder = folder_node(
            "parent".into(),
            "/tmp".into(),
            1,
            vec![file_node("other".into(), "/tmp/other".into(), 1), locked],
        );

        assert_eq!(node_lock_reason(&folder), None);
        assert_eq!(subtree_lock_reason(&folder), Some("Session is active"));
    }

    #[test]
    fn an_unlocked_subtree_reports_nothing() {
        let folder = folder_node(
            "parent".into(),
            "/tmp".into(),
            1,
            vec![file_node("child".into(), "/tmp/child".into(), 1)],
        );
        assert_eq!(subtree_lock_reason(&folder), None);
    }
}
