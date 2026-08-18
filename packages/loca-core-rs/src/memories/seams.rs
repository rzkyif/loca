//! What the extension supervisor reaches memories through.
//!
//! The three traits implemented here are DECLARED by `extensions::`, because
//! the consumer declaring them is what keeps the module graph acyclic: nothing
//! under `extensions/` imports a `memories::` type, so D4 can call into either
//! side without a cycle. The implementations belong here, next to the store
//! that owns the rows.
//!
//! # Disabled means absent, not hidden
//!
//! [`ToolMemoriesStore::get`] and [`ToolMemoriesStore::get_file`] answer
//! `not_found` for a memory the user disabled, with the same message a missing
//! title gets. Disabling is how a user takes a memory away from the model, so
//! anything short of absence hands it straight back: an agent that is told "it
//! exists but is off" has still been told the memory exists, and one that can
//! read it by title has been given the content the user withdrew.
//!
//! `write` and `edit` deliberately do NOT apply that rule, and the asymmetry is
//! the specification's (`module-broker.ts:191,200` check the flag; `:203` and
//! `:224` do not). Treating a disabled memory as absent on `write` would send
//! the call to `create`, which then fails the unique-title check with a
//! `conflict` a tool cannot act on, so the stricter reading is also the worse
//! one.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::contract::api::{Memory, MemoryKind};
use crate::contract::inbound::{MemoryDecl as DeclaredMemory, MemoryDeclKind};
use crate::error::{forbidden, not_found, Result};
use crate::extensions::broker::{MemoryBody, MemoryBrief, MemoryWrite, ToolMemories};
use crate::extensions::install::runner::ExtensionMemories;
use crate::extensions::ToolEmbedder;

use super::embedding::Embedding;
use super::indexer::MemoryIndexer;
use super::store::{kind_as_str, MemoriesStore, MemoryDecl};

/// The read-only memories an extension ships, as the installer registers them.
#[derive(Clone)]
pub struct ExtensionMemoriesStore {
    store: MemoriesStore,
    indexer: Arc<dyn MemoryIndexer>,
}

impl ExtensionMemoriesStore {
    pub fn new(store: MemoriesStore, indexer: Arc<dyn MemoryIndexer>) -> Self {
        Self { store, indexer }
    }
}

#[async_trait]
impl ExtensionMemories for ExtensionMemoriesStore {
    async fn register_extension_memories(
        &self,
        extension_id: &str,
        base_dir: &Path,
        memories: &[DeclaredMemory],
    ) -> Result<()> {
        let decls: Vec<MemoryDecl> = memories.iter().map(decl_from_manifest).collect();
        self.store
            .register_extension_memories(extension_id, base_dir.to_path_buf(), &decls)
            .await
    }

    async fn remove_extension_memories(&self, extension_id: &str) {
        // Infallible by declaration, and that is the right shape: this runs
        // inside an uninstall that has already removed the extension's files, so
        // a failure here leaves rows behind rather than something to roll back
        // to. Reported rather than raised.
        if let Err(err) = self.store.remove_extension_memories(extension_id).await {
            tracing::warn!(
                target: "loca::memories",
                "could not drop memories of extension \"{extension_id}\": {err}",
            );
        }
    }

    fn schedule_indexing(&self) {
        self.indexer.schedule(None);
    }
}

/// Translate one manifest declaration into the store's own shape.
///
/// Two enums with the same two members: the manifest's is generated from
/// `loca.json`'s schema, the store's is the persisted memory kind, and they are
/// separate types because either could gain a member the other must not.
fn decl_from_manifest(decl: &DeclaredMemory) -> MemoryDecl {
    MemoryDecl {
        kind: match decl.kind {
            MemoryDeclKind::Knowledge => MemoryKind::Knowledge,
            MemoryDeclKind::Skill => MemoryKind::Skill,
        },
        path: decl.path.to_string(),
    }
}

/// The tool-facing memories API: title-keyed, and blind to anything disabled.
#[derive(Clone)]
pub struct ToolMemoriesStore {
    store: MemoriesStore,
    indexer: Arc<dyn MemoryIndexer>,
}

impl ToolMemoriesStore {
    pub fn new(store: MemoriesStore, indexer: Arc<dyn MemoryIndexer>) -> Self {
        Self { store, indexer }
    }

    /// The memory `title` names, or `not_found` when there is none the model is
    /// allowed to see.
    ///
    /// A disabled memory takes the SAME error as a missing one, message
    /// included: a distinct message would leak that the title exists, which is
    /// half of what disabling withdrew.
    async fn readable(&self, title: &str) -> Result<Memory> {
        match self.store.get_by_title(title).await? {
            Some(memory) if memory.enabled => Ok(memory),
            _ => Err(not_found(format!("memory \"{title}\" not found"))),
        }
    }
}

#[async_trait]
impl ToolMemories for ToolMemoriesStore {
    async fn list(&self) -> Result<Vec<MemoryBrief>> {
        Ok(self
            .store
            .list()
            .await?
            .into_iter()
            .filter(|memory| memory.enabled)
            .map(|memory| MemoryBrief {
                title: memory.title,
                kind: kind_as_str(memory.kind).to_string(),
                summary: memory.summary.unwrap_or_default(),
                updated_at_ms: memory.updated_at_ms,
            })
            .collect())
    }

    async fn get(&self, title: &str) -> Result<MemoryBody> {
        let memory = self.readable(title).await?;
        Ok(MemoryBody {
            title: memory.title,
            content: memory.content,
        })
    }

    async fn get_file(&self, title: &str, name: &str) -> Result<MemoryBody> {
        let memory = self.readable(title).await?;
        let content = self.store.get_file(&memory.id, name).await?;
        Ok(MemoryBody {
            title: memory.title,
            content,
        })
    }

    async fn write(&self, title: &str, content: &str) -> Result<MemoryWrite> {
        let existing = self.store.get_by_title(title).await?;
        // A disabled memory is REFUSED, not treated as absent. This branch
        // returns `before`, so without the check a tool could read any disabled
        // memory's full content just by writing to its title, which defeats the
        // one control a user has for taking a memory away from the model.
        //
        // `forbidden` rather than `not_found`, which is what `get` answers:
        // "absent" would send this to `create`, and that fails the
        // case-insensitive unique-title check with a `conflict` the tool can
        // neither act on nor understand. This says the actionable thing.
        if existing.as_ref().is_some_and(|memory| !memory.enabled) {
            return Err(forbidden(format!("memory \"{title}\" is disabled")));
        }
        let before = existing
            .as_ref()
            .map(|memory| memory.content.clone())
            .unwrap_or_default();
        let memory = match &existing {
            Some(memory) => self.store.replace_content(&memory.id, content).await?,
            // Knowledge, never a skill: a skill is a folder with frontmatter a
            // tool has no way to author through a two-argument write.
            None => {
                self.store
                    .create(MemoryKind::Knowledge, title, content)
                    .await?
            }
        };
        self.indexer.schedule(Some(&memory.id));
        Ok(MemoryWrite {
            title: memory.title,
            before,
            after: content.to_string(),
            created: existing.is_none(),
        })
    }

    async fn edit(&self, title: &str, find: &str, replace: &str) -> Result<MemoryWrite> {
        let existing = self
            .store
            .get_by_title(title)
            .await?
            .ok_or_else(|| not_found(format!("memory \"{title}\" not found")))?;
        // Same reasoning as `write`: `edit_content` returns `before`, so a
        // disabled memory's content would leak through the result of an edit.
        if !existing.enabled {
            return Err(forbidden(format!("memory \"{title}\" is disabled")));
        }
        let outcome = self.store.edit_content(&existing.id, find, replace).await?;
        self.indexer.schedule(Some(&outcome.memory.id));
        Ok(MemoryWrite {
            title: outcome.memory.title,
            before: outcome.before,
            after: outcome.after,
            created: false,
        })
    }
}

/// Embeddings, for the tool-relevance routes.
#[derive(Clone)]
pub struct ToolEmbedding {
    embedding: Embedding,
}

impl ToolEmbedding {
    pub fn new(embedding: Embedding) -> Self {
        Self { embedding }
    }
}

#[async_trait]
impl ToolEmbedder for ToolEmbedding {
    async fn is_ready(&self) -> bool {
        self.embedding.is_ready().await
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embedding.embed(texts).await
    }

    async fn embed_with_hash(&self, text: &str) -> Result<Option<(Vec<f32>, String)>> {
        Ok(self
            .embedding
            .embed_with_hash(text)
            .await?
            .map(|embedded| (embedded.vector, embedded.source_hash)))
    }

    fn source_hash(&self, text: &str) -> String {
        self.embedding.source_hash(text)
    }
}

#[cfg(test)]
mod tests;
