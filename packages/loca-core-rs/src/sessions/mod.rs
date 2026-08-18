//! Sessions and messages: the on-disk store, chat history, attachments, and routes.
//!
//! Work package **D2** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! Sessions are **JSON files on disk, not SQLite**, one directory per session
//! under `<root>/sessions/`. That is deliberate and is documented in the
//! database schema. The tree is one of the byte-exact parity surfaces: an
//! existing install's `sessions/` is read as-is, so every filename and field
//! name here is fixed by what is already on disk.
//!
//! Two shapes in here are worth knowing before reading the rest:
//!
//! - A stored message is a raw JSON map, not the generated `Message` enum. See
//!   [`doc::StoredMessage`] for why that is a correctness decision rather than a
//!   convenience.
//! - The chat orchestrator is reached through the [`chat_engine::ChatEngine`]
//!   seam, declared here and implemented by D4, so the SSE route compiles and is
//!   testable without a language model.

pub mod attachments;
pub mod chat_engine;
pub mod doc;
pub mod history;
pub mod routes;
pub mod store;
pub mod summary;

#[cfg(test)]
mod tests;

pub use chat_engine::{ChatEngine, OneShotDelta, OneShotRequest, UnavailableChat};
pub use doc::{AttachmentRecord, SessionDoc, SessionMeta, StoredMessage};
pub use history::{last_user_id, last_user_text};
pub use store::{
    Appended, CreateSessionInput, DeletedSession, NewAttachment, SessionFootprint, SessionsStore,
};
pub use summary::content_to_text;
