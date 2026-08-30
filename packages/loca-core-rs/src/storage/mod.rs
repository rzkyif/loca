//! The storage browser: what is on disk and what can be reclaimed.
//!
//! Core is the sole authority on what may be removed. Every row is tagged with
//! a `lock_reason` when it is in use, and a reclaim request re-derives that lock
//! from the state at the moment of the request, so a stale or tampered client
//! view cannot force a delete of something in use.
//!
//! # Shape
//!
//! **Facts in, walk pure, mutations out.**
//!
//! 1. [`Facts::gather`] awaits every seam once and produces plain values.
//! 2. Building the tree is a synchronous function over the filesystem, run on
//!    the blocking pool. It takes no trait objects, so every walk and lock rule
//!    is testable with literal facts over a tempdir.
//! 3. Removals that belong to another subsystem go back out through the seams.
//!
//! The walk runs on the blocking pool because it is the only thing in the core
//! that can spend seconds in the filesystem: `GET /api/v1/storage` stats every
//! file under the models directory, the three cache directories and every
//! installed extension, on every request. Nothing is cached, deliberately: a
//! cached tree on the delete path would defeat the point of re-deriving the
//! lock, and the index a delete resolves against has to describe the disk now.

pub mod categories;
pub mod deps;
pub mod facts;
mod models;
mod node;
pub mod routes;
mod safety;
mod walk;

use std::collections::HashMap;
use std::sync::Arc;

use crate::contract::api::{StorageCategory, StorageCategoryId, StorageNode, StorageTree};
use crate::error::{internal, validation, Result};
use crate::paths::Paths;
use crate::secrets::Vault;
use crate::transport::state::AppContext;

use deps::StorageDeps;
use facts::Facts;
use node::{node_path, node_size, subtree_lock_reason};

/// The storage view and its reclaim operations.
#[derive(Clone)]
pub struct StorageService {
    paths: Arc<Paths>,
    vault: Vault,
    deps: StorageDeps,
}

impl StorageService {
    pub fn new(ctx: &AppContext, deps: StorageDeps) -> Self {
        Self {
            paths: Arc::clone(&ctx.paths),
            vault: ctx.vault.clone(),
            deps,
        }
    }

    /// The whole tree: every category, its rows, and their sizes.
    pub async fn tree(&self) -> Result<StorageTree> {
        let facts = Facts::gather(&self.deps).await;
        let paths = Arc::clone(&self.paths);
        blocking(move || Ok(build_tree(&paths, &facts))).await
    }

    /// Reclaim the given paths.
    ///
    /// Each must match a row in the freshly built tree, belong to a deletable
    /// category, and be free of any locked descendant. A path that matches
    /// nothing is skipped, which is what makes a repeated request idempotent
    /// and what makes a traversal attempt a no-op rather than an error.
    ///
    /// Every target is resolved and checked before anything is removed, so a
    /// request naming one locked row removes nothing at all.
    pub async fn delete_paths(&self, targets: &[String]) -> Result<()> {
        let index = self.index().await?;
        let mut planned = Vec::new();
        for target in targets {
            let Some((category, node)) = index.get(&safety::normalize(target)) else {
                continue; // already gone, or never ours
            };
            if !categories::deletable(*category) {
                return Err(validation(format!("category {category} is not deletable")));
            }
            if subtree_lock_reason(node).is_some() {
                return Err(validation(format!(
                    "cannot delete in-use item: {}",
                    node_path(node)
                )));
            }
            planned.push((*category, node.clone()));
        }

        for (category, node) in planned {
            // `node` came out of the index, so this is a path Core enumerated,
            // never the string the caller sent. Do not "simplify" this into
            // deleting the requested path.
            if let Err(err) = self.remove(category, &node).await {
                tracing::warn!(target: "loca::storage", "delete {} failed: {}", node_path(&node), err.message);
                return Err(err);
            }
        }
        self.deps.requirements.changed();
        Ok(())
    }

    /// Empty one category of every row that is not in use.
    ///
    /// Unlike [`Self::delete_paths`], one row failing does not abandon the
    /// rest: a clear is a best-effort sweep, and stopping at the first
    /// stubborn file would leave the user with no way to finish.
    pub async fn clear_category(&self, category_id: &str) -> Result<()> {
        let category: StorageCategoryId = category_id
            .parse()
            .map_err(|_| validation(format!("unknown storage category: {category_id}")))?;
        if !categories::deletable(category) {
            return Err(validation(format!("category {category} is not deletable")));
        }

        let facts = Facts::gather(&self.deps).await;
        let paths = Arc::clone(&self.paths);
        let nodes = blocking(move || Ok(categories::build_nodes(category, &paths, &facts))).await?;

        for node in nodes {
            if subtree_lock_reason(&node).is_some() {
                continue;
            }
            if let Err(err) = self.remove(category, &node).await {
                tracing::warn!(target: "loca::storage", "clear {} failed: {}", node_path(&node), err.message);
            }
        }
        self.deps.requirements.changed();
        Ok(())
    }

    async fn remove(&self, category: StorageCategoryId, node: &StorageNode) -> Result<()> {
        categories::delete_node(category, node, &self.paths, &self.vault, &self.deps).await
    }

    /// Every row in the tree, keyed by normalised path, so a requested target
    /// can be resolved to the node Core itself produced.
    async fn index(&self) -> Result<HashMap<String, (StorageCategoryId, StorageNode)>> {
        let tree = self.tree().await?;
        let mut index = HashMap::new();
        for category in tree.categories {
            for node in category.nodes {
                insert_node(&mut index, category.id, node);
            }
        }
        Ok(index)
    }
}

fn insert_node(
    index: &mut HashMap<String, (StorageCategoryId, StorageNode)>,
    category: StorageCategoryId,
    node: StorageNode,
) {
    if let StorageNode::Folder { children, .. } = &node {
        for child in children.clone() {
            insert_node(index, category, child);
        }
    }
    index.insert(safety::normalize(node_path(&node)), (category, node));
}

fn build_tree(paths: &Paths, facts: &Facts) -> StorageTree {
    let categories: Vec<StorageCategory> = categories::ALL
        .into_iter()
        .map(|id| {
            let nodes = categories::build_nodes(id, paths, facts);
            let size = nodes.iter().map(node_size).sum();
            StorageCategory {
                id,
                label: categories::label(id).to_string(),
                deletable: categories::deletable(id),
                nodes,
                size,
            }
        })
        .collect();

    StorageTree {
        total_size: categories.iter().map(|category| category.size).sum(),
        categories,
        root_path: root_path(paths),
    }
}

/// The directory whose `models` child holds the model files, which is what lets
/// the client map a HuggingFace spec to a path on disk.
///
/// On a default install this is `~/.loca`, **not** the channel root, because
/// models are shared across channels while everything else is not. The two
/// coincide whenever the root is overridden, which every tempdir test does, so
/// no test here can catch this returning `paths.root()` instead.
fn root_path(paths: &Paths) -> String {
    paths
        .models_dir()
        .parent()
        .unwrap_or_else(|| paths.models_dir())
        .to_string_lossy()
        .into_owned()
}

/// Run filesystem work on the blocking pool.
async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| internal(format!("storage task failed: {err}")))?
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
