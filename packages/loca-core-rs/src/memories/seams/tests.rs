//! SEAMS' suite for the memories side of the extension supervisor's seams.
//!
//! The disabled-memory cases are the reason this file exists. Each of them
//! stages the SAME memory twice, enabled and then disabled, and asserts the
//! answer flips: a test that only ever looks at a disabled memory would still
//! pass against an implementation that answers `not_found` for everything.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use tempfile::TempDir;

use crate::contract::api::MemoryKind;
use crate::contract::inbound::{MemoryDecl as DeclaredMemory, MemoryDeclKind, MemoryDeclPath};
use crate::db::{migrate, Db};
use crate::error::ErrorCode;
use crate::extensions::broker::ToolMemories;
use crate::extensions::install::runner::ExtensionMemories;
use crate::memories::indexer::MemoryIndexer;
use crate::memories::store::MemoriesStore;
use crate::paths::Paths;

use super::{ExtensionMemoriesStore, ToolMemoriesStore};

struct Env {
    _tmp: TempDir,
    root: std::path::PathBuf,
    store: MemoriesStore,
    tools: ToolMemoriesStore,
    shipped: ExtensionMemoriesStore,
    indexer: Arc<Recorder>,
}

/// Records what was scheduled, so a test can tell a pass that indexed from one
/// that only looked like it.
#[derive(Default)]
struct Recorder {
    scheduled: std::sync::Mutex<Vec<Option<String>>>,
}

impl MemoryIndexer for Recorder {
    fn schedule(&self, memory_id: Option<&str>) {
        self.scheduled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(memory_id.map(str::to_string));
    }
}

impl Recorder {
    fn taken(&self) -> Vec<Option<String>> {
        self.scheduled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path());
    paths.ensure_dirs().unwrap();
    let paths = Arc::new(paths);
    let db = Db::open_in_memory().unwrap();
    db.call_blocking(|conn| migrate(conn)).unwrap();
    let store = MemoriesStore::new(Arc::clone(&paths), db);
    let indexer = Arc::new(Recorder::default());
    Env {
        root: tmp.path().to_path_buf(),
        _tmp: tmp,
        tools: ToolMemoriesStore::new(store.clone(), indexer.clone()),
        shipped: ExtensionMemoriesStore::new(store.clone(), indexer.clone()),
        store,
        indexer,
    }
}

// --- disabled means absent ---------------------------------------------------

#[tokio::test]
async fn get_stops_answering_once_the_memory_is_disabled() {
    let env = env();
    let memory = env
        .store
        .create(MemoryKind::Knowledge, "Vault Code", "the code is 1234")
        .await
        .unwrap();

    // Enabled: readable, and the STORE's title comes back rather than the
    // caller's spelling.
    let body = env.tools.get("vault code").await.unwrap();
    assert_eq!(body.title, "Vault Code");
    assert_eq!(body.content, "the code is 1234");

    env.store.set_enabled(&memory.id, false).await.unwrap();

    let err = env.tools.get("Vault Code").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    // The message must not distinguish a disabled memory from a missing one:
    // naming it as disabled would leak the existence the user withdrew.
    assert_eq!(err.message, "memory \"Vault Code\" not found");
    assert_eq!(
        env.tools.get("no such memory").await.unwrap_err().message,
        "memory \"no such memory\" not found",
    );
    // The content is still there; it is the tool-facing read that stopped.
    assert!(env
        .store
        .get(&memory.id)
        .await
        .unwrap()
        .content
        .contains("1234"));
}

#[tokio::test]
async fn get_file_stops_answering_once_the_memory_is_disabled() {
    let env = env();
    let memory = env
        .store
        .create(
            MemoryKind::Skill,
            "Deploy",
            "---\ndescription: ship it\n---\n",
        )
        .await
        .unwrap();
    env.store
        .write_file(&memory.id, "runbook.md", "step one")
        .await
        .unwrap();

    let body = env.tools.get_file("Deploy", "runbook.md").await.unwrap();
    assert_eq!(body.title, "Deploy");
    assert_eq!(body.content, "step one");

    env.store.set_enabled(&memory.id, false).await.unwrap();

    let err = env
        .tools
        .get_file("Deploy", "runbook.md")
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert_eq!(err.message, "memory \"Deploy\" not found");
}

#[tokio::test]
async fn list_reports_only_enabled_memories() {
    let env = env();
    let kept = env
        .store
        .create(MemoryKind::Knowledge, "Kept", "kept body")
        .await
        .unwrap();
    let hidden = env
        .store
        .create(MemoryKind::Knowledge, "Hidden", "hidden body")
        .await
        .unwrap();
    assert_eq!(env.tools.list().await.unwrap().len(), 2);

    env.store.set_enabled(&hidden.id, false).await.unwrap();

    let listed = env.tools.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "Kept");
    assert_eq!(listed[0].kind, "knowledge");
    assert_eq!(listed[0].updated_at_ms, kept.updated_at_ms);
}

// --- write and edit ----------------------------------------------------------

#[tokio::test]
async fn write_creates_then_replaces_and_indexes_each_time() {
    let env = env();

    let created = env.tools.write("Notes", "first").await.unwrap();
    assert!(created.created);
    assert_eq!(created.title, "Notes");
    assert_eq!(created.before, "");
    assert_eq!(created.after, "first");

    let replaced = env.tools.write("notes", "second").await.unwrap();
    assert!(!replaced.created);
    // The stored title, not the caller's re-casing.
    assert_eq!(replaced.title, "Notes");
    assert_eq!(replaced.before, "first");
    assert_eq!(replaced.after, "second");

    let id = env.store.get_by_title("Notes").await.unwrap().unwrap().id;
    assert_eq!(
        env.indexer.taken(),
        vec![Some(id.clone()), Some(id)],
        "both writes schedule an index pass for the row they touched",
    );
}

#[tokio::test]
async fn edit_refuses_a_missing_title_and_indexes_a_real_one() {
    let env = env();
    let err = env.tools.edit("Nope", "a", "b").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(env.indexer.taken().is_empty());

    let memory = env
        .store
        .create(MemoryKind::Knowledge, "Notes", "alpha beta")
        .await
        .unwrap();
    let out = env.tools.edit("Notes", "beta", "gamma").await.unwrap();
    assert_eq!(out.before, "alpha beta");
    assert_eq!(out.after, "alpha gamma");
    assert!(!out.created);
    assert_eq!(env.indexer.taken(), vec![Some(memory.id)]);
}

// --- what an extension ships -------------------------------------------------

fn decl(kind: MemoryDeclKind, path: &str) -> DeclaredMemory {
    DeclaredMemory {
        kind,
        path: MemoryDeclPath::try_from(path).unwrap(),
    }
}

/// `base_dir` is the PARENT of the install directory, exactly as the installer
/// passes it.
fn extensions_base(env: &Env) -> std::path::PathBuf {
    env.root.join("extensions")
}

#[tokio::test]
async fn shipped_memories_register_disabled_under_the_extension_provider() {
    let env = env();
    let base = extensions_base(&env);
    let install = base.join("acme");
    std::fs::create_dir_all(install.join("skills/deploy")).unwrap();
    std::fs::write(install.join("guide.md"), "read me").unwrap();
    std::fs::write(
        install.join("skills/deploy/SKILL.md"),
        "---\ndescription: ships it\nsuggestedTools: [bash]\n---\nbody\n",
    )
    .unwrap();

    env.shipped
        .register_extension_memories(
            "acme",
            &base,
            &[
                decl(MemoryDeclKind::Knowledge, "guide.md"),
                decl(MemoryDeclKind::Skill, "skills/deploy"),
                // Never written: skipped rather than failing the install.
                decl(MemoryDeclKind::Knowledge, "absent.md"),
            ],
        )
        .await
        .unwrap();

    let rows = env.store.all_rows().await.unwrap();
    assert_eq!(rows.len(), 2, "the missing declaration is skipped");
    for row in &rows {
        assert_eq!(row.provider, "acme");
        assert!(
            !row.enabled,
            "an extension's memory arrives opt-in, so it must be disabled",
        );
        assert!(row.filename.starts_with("acme/"));
    }

    // The kind survived the manifest-to-store translation, and the skill's
    // frontmatter seeded the summary.
    let skill = rows.iter().find(|r| r.kind == MemoryKind::Skill).unwrap();
    assert_eq!(skill.filename, "acme/skills/deploy");
    assert_eq!(skill.summary.as_deref(), Some("ships it"));
    assert_eq!(
        skill.suggested_tools.as_deref(),
        Some(&["bash".to_string()][..])
    );

    // Content resolves back inside the install directory, which is the whole
    // point of `base_dir` being the parent.
    let knowledge = rows
        .iter()
        .find(|r| r.kind == MemoryKind::Knowledge)
        .unwrap();
    assert_eq!(
        env.store.get(&knowledge.id).await.unwrap().content,
        "read me"
    );
    assert!(!env
        .tools
        .list()
        .await
        .unwrap()
        .iter()
        .any(|m| m.title == "Guide"));
}

#[tokio::test]
async fn a_shipped_path_that_escapes_the_install_dir_is_skipped_not_fatal() {
    let env = env();
    let base = extensions_base(&env);
    std::fs::create_dir_all(base.join("acme")).unwrap();
    std::fs::write(base.join("acme/inside.md"), "fine").unwrap();
    std::fs::write(base.join("outside.md"), "secret").unwrap();

    env.shipped
        .register_extension_memories(
            "acme",
            &base,
            &[
                decl(MemoryDeclKind::Knowledge, "../outside.md"),
                decl(MemoryDeclKind::Knowledge, "inside.md"),
            ],
        )
        .await
        .expect("an escaping path is skipped with a warning, not an install failure");

    let rows = env.store.all_rows().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].filename, "acme/inside.md");
}

#[tokio::test]
async fn removing_an_extension_drops_its_rows_and_leaves_the_users_alone() {
    let env = env();
    let base = extensions_base(&env);
    std::fs::create_dir_all(base.join("acme")).unwrap();
    std::fs::write(base.join("acme/guide.md"), "read me").unwrap();
    env.store
        .create(MemoryKind::Knowledge, "Mine", "user body")
        .await
        .unwrap();
    env.shipped
        .register_extension_memories(
            "acme",
            &base,
            &[decl(MemoryDeclKind::Knowledge, "guide.md")],
        )
        .await
        .unwrap();
    assert_eq!(env.store.all_rows().await.unwrap().len(), 2);

    env.shipped.remove_extension_memories("acme").await;

    let rows = env.store.all_rows().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Mine");
}

#[tokio::test]
async fn schedule_indexing_asks_for_a_full_pass() {
    let env = env();
    env.shipped.schedule_indexing();
    assert_eq!(env.indexer.taken(), vec![None]);
}

#[tokio::test]
async fn a_disabled_memory_is_refused_by_write_and_edit_and_leaks_nothing() {
    // `get`/`get_file` already answer `not_found` for a disabled memory, but
    // `write` and `edit` both return the prior content as `before`, and neither
    // checked the flag. So a tool could read any disabled memory in full simply
    // by writing to its title, which is precisely what disabling exists to
    // prevent. Ruled by the repo owner: refuse with `forbidden` rather than
    // pretend it is absent, because "absent" sends `write` to `create` and that
    // fails the unique-title check with a `conflict` a tool cannot act on.
    let env = env();
    let memory = env
        .store
        .create(MemoryKind::Knowledge, "Secrets", "the private body")
        .await
        .unwrap();
    env.store.set_enabled(&memory.id, false).await.unwrap();

    let write_err = env.tools.write("Secrets", "overwrite").await.unwrap_err();
    let edit_err = env
        .tools
        .edit("Secrets", "private", "public")
        .await
        .unwrap_err();

    for err in [&write_err, &edit_err] {
        assert_eq!(err.code, ErrorCode::Forbidden);
        // Asserted over the whole rendered error rather than the code alone: a
        // message that interpolated the body would satisfy a code-only check.
        assert!(
            !format!("{err:?}").contains("the private body"),
            "the refusal leaked the disabled memory's content"
        );
    }

    // Enabled again, both ops work. Without this the test would also pass
    // against an implementation that refused every write.
    env.store.set_enabled(&memory.id, true).await.unwrap();
    let written = env.tools.write("Secrets", "a new body").await.unwrap();
    assert_eq!(written.before, "the private body");
    assert!(!written.created);
}
