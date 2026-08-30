//! Memories, embeddings, and relevance-based injection.
//!
//! Markdown under `<root>/memories/` is the source of truth (`<slug>.md` for
//! knowledge, `<slug>/SKILL.md` for skills); SQLite holds metadata plus the
//! background-generated summary and embedding, reconciled by content hash.
//! Extension-shipped memories are read-only and live under the extension's
//! install directory.
//!
//! Two invisible breakages: relevance arithmetic must accumulate in `f64`
//! (see [`relevance`]), and [`embedding::EMBED_REPO`] is hashed into every
//! stored `embedding_source_hash`, so respelling it marks every vector stale.

pub mod content;
pub mod embedding;
pub mod indexer;
pub mod injection;
pub mod relevance;
pub mod routes;
pub mod seams;
pub mod skill;
pub mod store;

#[cfg(test)]
mod tests;

pub use crate::contract::api::{Memory, MemoryKind, MemoryMeta};
pub use embedding::{Embedding, LocalEmbedder, NoLocalEmbedder, EMBED_REPO};
pub use indexer::{MemoryIndexer, NoIndexer, SummaryGenerator};
pub use injection::{
    has_memories, memory_token_blocks, relevant_memories, relevant_memories_block,
    MemoryTokenBudget, RelevantMemory,
};
pub use seams::{ExtensionMemoriesStore, ToolEmbedding, ToolMemoriesStore};
pub use store::{MemoriesStore, MemoryDecl, ProviderGate, USER_MEMORY_PROVIDER};
