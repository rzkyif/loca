//! Memories, embeddings, and relevance-based injection.
//!
//! Work package **D5** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! # What is where
//!
//! Reference *knowledge* (a `<slug>.md` file) and procedural *skills* (a
//! `<slug>/` folder holding `SKILL.md` plus optional bundled files) under
//! `<root>/memories/` are **the source of truth**. SQLite carries the metadata
//! plus the background-generated summary and embedding, and [`store`]'s
//! `rescan` reconciles the two by content hash, so a memory edited or dropped in
//! by hand becomes first-class on the next scan. A memory shipped by an
//! extension lives under that extension's install directory, is read-only, and
//! is registered through [`store::MemoriesStore::register_extension_memories`].
//!
//! | module        | what                                                            |
//! | ------------- | --------------------------------------------------------------- |
//! | [`store`]     | CRUD, rescan, extension memories, index bookkeeping             |
//! | [`content`]   | the on-disk layout: paths, slugs, titles, atomic writes         |
//! | [`skill`]     | the `SKILL.md` frontmatter grammar                              |
//! | [`relevance`] | cosine, top-K, the staleness hash, the embed-text builders      |
//! | [`embedding`] | the embedding service and the local-engine seam                 |
//! | [`injection`] | what a chat turn injects: relevant summaries and `@token` bodies |
//! | [`indexer`]   | the background pass that fills `summary` and `embedding`        |
//! | [`routes`]    | `/api/v1/memories`                                              |
//! | [`seams`]     | what the extension supervisor reaches all of the above through  |
//!
//! # Two things that are easy to break invisibly
//!
//! **The arithmetic.** Cosine and L2 normalisation are defined by the exact
//! numbers they produce, not by their shape; an `f32` accumulator where
//! JavaScript uses `f64` changes rankings without changing anything a shape
//! assertion would catch. [`relevance`] says how, and the fixture that pins it
//! refuses to be compared approximately.
//!
//! **The embedding model's identity.** It is hashed into every stored
//! `embedding_source_hash`, so a core that spells it differently reads an
//! existing install's entire vector store as stale and silently stops injecting
//! memories until a full reindex completes. [`embedding::EMBED_REPO`] is that
//! string, and a test pins it.

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
