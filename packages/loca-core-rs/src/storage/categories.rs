//! The seven categories: what each one lists, and what removing one of its
//! rows means.
//!
//! In-use rules, which is the whole of what "locked" means here:
//!
//! - **models**: required by the current settings
//! - **binaries**: required by the settings, or run by a live sidecar
//! - **cache**: never, it regenerates
//! - **logs**: never, the live log is truncated rather than removed
//! - **sessions**: an agent is mid-turn on it
//! - **extensions**: it was seeded, or it has an enabled tool
//! - **settings**: never, because clearing it is a factory reset rather than a
//!   file removal
//!
//! Every lock string lives here rather than behind a seam. The subsystems that
//! own the facts do not own the wording a user reads.

use std::path::Path;

use crate::contract::api::{StorageCategoryId, StorageNode};
use crate::error::Result;
use crate::paths::Paths;
use crate::secrets::Vault;

use super::deps::StorageDeps;
use super::facts::Facts;
use super::node::{file_node, leaf_folder, node_path, with_lock};
use super::safety::{assert_under, remove_path};
use super::walk::{by_name, dir_size, stat_size};
use super::{models, node};

/// Render order, which is the order the client draws its groups in and is
/// therefore contractual. It is not the order the generated enum declares its
/// variants in.
pub const ALL: [StorageCategoryId; 7] = [
    StorageCategoryId::Models,
    StorageCategoryId::Binaries,
    StorageCategoryId::Cache,
    StorageCategoryId::Logs,
    StorageCategoryId::Sessions,
    StorageCategoryId::Extensions,
    StorageCategoryId::Settings,
];

pub fn label(category: StorageCategoryId) -> &'static str {
    match category {
        StorageCategoryId::Models => "Models",
        StorageCategoryId::Binaries => "Binaries",
        StorageCategoryId::Cache => "Cache",
        StorageCategoryId::Logs => "Logs",
        StorageCategoryId::Sessions => "Sessions",
        StorageCategoryId::Extensions => "Extensions",
        StorageCategoryId::Settings => "Settings",
    }
}

/// Whether a category supports reclaiming at all.
///
/// Every category does today. The check stays because it is what gives the
/// `deletable` field on the wire its meaning: without it, flipping the flag
/// would hide the rows in the client and still delete them if asked.
pub fn deletable(_category: StorageCategoryId) -> bool {
    true
}

/// Build one category's rows.
///
/// Total by construction: every filesystem failure inside is already a zero or an
/// empty list, and everything a seam owns arrived in [`Facts`] already degraded.
/// A broken corner of the tree yields an empty category rather than an error.
pub fn build_nodes(category: StorageCategoryId, paths: &Paths, facts: &Facts) -> Vec<StorageNode> {
    match category {
        StorageCategoryId::Models => match &facts.required_model_paths {
            Some(required) => models::build_nodes(paths.models_dir(), required),
            // The lock source is unreachable, so every row would claim to be
            // safe to delete. Showing nothing is the honest answer.
            None => Vec::new(),
        },
        StorageCategoryId::Binaries => facts
            .binaries
            .iter()
            .map(|binary| {
                let lock = facts
                    .locked_binary_kinds
                    .contains(&binary.kind)
                    .then(|| "Required by an enabled feature".to_string());
                with_lock(
                    file_node(
                        binary.kind.clone(),
                        binary.path.clone(),
                        stat_size(&binary.path),
                    ),
                    lock,
                )
            })
            .collect(),
        StorageCategoryId::Cache => [
            ("cache", paths.cache_dir()),
            ("deno-cache", paths.deno_cache_dir()),
            ("staging", paths.staging_dir()),
        ]
        .into_iter()
        .map(|(name, dir)| leaf_folder(name.to_string(), dir.clone(), dir_size(&dir)))
        .collect(),
        StorageCategoryId::Logs => {
            let mut nodes: Vec<StorageNode> = read_files(&paths.logs_dir())
                .into_iter()
                .map(|(name, path)| {
                    let size = stat_size(&path);
                    // Every log is clearable. The live one is truncated rather
                    // than removed, which is the delete path's business.
                    file_node(name, path, size)
                })
                .collect();
            nodes.sort_by(|a, b| by_name(node::node_name(a), node::node_name(b)));
            nodes
        }
        StorageCategoryId::Sessions => {
            let mut sessions = facts.sessions.clone();
            sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at_ms));
            sessions
                .into_iter()
                .map(|session| {
                    let title = if session.title.is_empty() {
                        "Untitled session".to_string()
                    } else {
                        session.title.clone()
                    };
                    let lock = facts
                        .active_session_ids
                        .contains(&session.id)
                        .then(|| "Session is active".to_string());
                    with_lock(
                        leaf_folder(title, paths.session_dir(&session.id), session.size_bytes),
                        lock,
                    )
                })
                .collect()
        }
        StorageCategoryId::Extensions => {
            let mut nodes: Vec<StorageNode> = facts
                .extensions
                .iter()
                .map(|extension| {
                    let dir = paths.extension_dir(&extension.id);
                    let name = if extension.display_name.is_empty() {
                        extension.id.clone()
                    } else {
                        extension.display_name.clone()
                    };
                    let lock = if extension.seeded {
                        Some(if extension.builtin {
                            "Built-in extension".to_string()
                        } else {
                            "Seeded extension".to_string()
                        })
                    } else if extension.has_enabled_tools {
                        Some("Has enabled tools".to_string())
                    } else {
                        None
                    };
                    with_lock(leaf_folder(name, dir.clone(), dir_size(&dir)), lock)
                })
                .collect();
            nodes.sort_by(|a, b| by_name(node::node_name(a), node::node_name(b)));
            nodes
        }
        StorageCategoryId::Settings => {
            let size = stat_size(&paths.settings_file()) + stat_size(&paths.secrets_enc_file());
            // Never locked: reclaiming it is a factory reset, not a removal.
            vec![file_node(
                "Settings + secrets".to_string(),
                paths.settings_file(),
                size,
            )]
        }
    }
}

/// Reclaim one row.
///
/// `node` is always a node this core enumerated, never a path a caller sent.
/// See the module docs on `safety` for why that distinction is the whole of the
/// traversal guard.
pub async fn delete_node(
    category: StorageCategoryId,
    node: &StorageNode,
    paths: &Paths,
    vault: &Vault,
    deps: &StorageDeps,
) -> Result<()> {
    let path = std::path::PathBuf::from(node_path(node));
    match category {
        StorageCategoryId::Models => {
            let root = paths.models_dir().to_path_buf();
            super::blocking(move || {
                assert_under(&path, &root)?;
                remove_path(&path)
            })
            .await
        }
        StorageCategoryId::Binaries => {
            let root = paths.bin_dir();
            super::blocking(move || {
                assert_under(&path, &root)?;
                remove_path(&path)
            })
            .await
        }
        StorageCategoryId::Cache => {
            super::blocking(move || {
                remove_path(&path)?;
                // Recreated because `ensure_dirs` makes these at boot and the
                // rest of the core expects them to exist for the whole run.
                std::fs::create_dir_all(&path)?;
                Ok(())
            })
            .await
        }
        StorageCategoryId::Logs => {
            let root = paths.logs_dir();
            let live = paths.log_file();
            super::blocking(move || {
                assert_under(&path, &root)?;
                if super::safety::normalize_path(&path) == super::safety::normalize_path(&live) {
                    // Removing the open log would reclaim nothing on Unix,
                    // where the logger keeps the inode, and would fail
                    // outright on Windows. Truncating empties it now and the
                    // logger keeps appending from zero.
                    let _ = std::fs::OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(&path);
                    Ok(())
                } else {
                    remove_path(&path)
                }
            })
            .await
        }
        StorageCategoryId::Sessions => deps.sessions.delete(&directory_id(&path)).await,
        StorageCategoryId::Extensions => deps.extensions.delete(&directory_id(&path)).await,
        StorageCategoryId::Settings => {
            deps.settings.factory_reset().await?;
            vault.clear_all().await
        }
    }
}

/// The id a session or extension row stands for, which is the last component
/// of its directory.
fn directory_id(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Every regular file directly inside `dir`, as `(name, path)`. Subdirectories
/// and symlinks are skipped, and an unreadable directory is empty.
fn read_files(dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect()
}
