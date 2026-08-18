//! Reclaim-path tests.
//!
//! # How these avoid risking a real tree
//!
//! Every fixture is a `tempfile::tempdir` holding two siblings: `root/`, which
//! the core root is rooted at, and `outside/canary.txt`, which nothing in the
//! tree may ever reach. Because `Paths::rooted` puts the models directory
//! inside the root too, no path this module can produce escapes the tempdir
//! even if every guard failed at once, and `TempDir` removes the lot on drop.
//! Each destructive test asserts the canary survived, so an escape fails an
//! assertion instead of passing over an empty tree.
//!
//! # Why the guards are tested separately
//!
//! The membership guard alone defeats every traversal input, so an end-to-end
//! escape test passes with `assert_under` deleted. Each guard therefore has a
//! test that fails when only that guard is removed, and each was watched go red
//! against a mutated build before being kept (D-015):
//!
//! - `assert_under` itself, in `safety.rs`, against roots and escapes directly.
//! - membership, here, with a real file inside the root that no category lists.
//! - the lock, here, on a row the facts mark as in use.
//! - the ordering, here, on a request that mixes a locked and an unlocked row.
//! - the symlink skip, in `walk.rs` and `models.rs`, on the size and on the
//!   node list, which are the two things that decide what can be deleted.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::config::Channel;
use crate::contract::api::StorageCategoryId;
use crate::error::Result;

use super::deps::{ModelLocks, SessionSummary, SessionsSource, StorageDeps};
use super::*;
use super::{node, walk};

// --- fixture -----------------------------------------------------------------

struct Fixture {
    tmp: tempfile::TempDir,
    service: StorageService,
}

impl Fixture {
    fn new(deps: StorageDeps) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = Paths::rooted(tmp.path().join("root"));
        paths.ensure_dirs().expect("ensure_dirs");
        std::fs::create_dir_all(tmp.path().join("outside")).expect("outside");
        std::fs::write(tmp.path().join("outside/canary.txt"), b"do not touch").expect("canary");

        let vault = Vault::with_store(
            paths.secrets_enc_file(),
            Arc::new(crate::secrets::testing::MemoryStore::new()),
        );
        Self {
            tmp,
            service: StorageService {
                paths: Arc::new(paths),
                vault,
                deps,
            },
        }
    }

    /// A core with the model lock source wired and nothing required, which is
    /// what makes the models category visible at all.
    fn with_models() -> Self {
        Self::new(StorageDeps::unwired().with_models(Arc::new(Locks(Vec::new()))))
    }

    fn paths(&self) -> &Paths {
        &self.service.paths
    }

    /// Replace the seams after the tree exists, for a test whose lock source
    /// has to name a path it could only learn by creating it first.
    fn rewire(&mut self, deps: StorageDeps) {
        self.service.deps = deps;
    }

    fn canary(&self) -> PathBuf {
        self.tmp.path().join("outside/canary.txt")
    }

    fn assert_canary_intact(&self) {
        assert!(
            self.canary().exists(),
            "a path outside the core root was removed"
        );
    }

    fn write(&self, path: &std::path::Path, len: usize) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, vec![0u8; len]).expect("write");
    }

    fn model(&self, relative: &str, len: usize) -> PathBuf {
        let path = self.paths().models_dir().join(relative);
        self.write(&path, len);
        path
    }

    async fn category(&self, id: StorageCategoryId) -> crate::contract::api::StorageCategory {
        let tree = self.service.tree().await.expect("tree");
        tree.categories
            .into_iter()
            .find(|category| category.id == id)
            .expect("category present")
    }
}

// --- seam doubles ------------------------------------------------------------

/// A wired model lock source, reporting exactly the paths it was given.
struct Locks(Vec<PathBuf>);

#[async_trait]
impl ModelLocks for Locks {
    async fn required_model_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(self.0.clone())
    }
}

/// A wired sessions source over an in-memory list, recording what it was asked
/// to delete.
struct Sessions {
    listed: Vec<SessionSummary>,
    active: HashSet<String>,
    deleted: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl SessionsSource for Sessions {
    async fn list(&self) -> Result<Vec<SessionSummary>> {
        Ok(self.listed.clone())
    }

    async fn active_ids(&self) -> Result<HashSet<String>> {
        Ok(self.active.clone())
    }

    async fn delete(&self, session_id: &str) -> Result<()> {
        self.deleted
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(session_id.to_string());
        Ok(())
    }
}

fn session(id: &str, title: &str, updated_at_ms: i64) -> SessionSummary {
    SessionSummary {
        id: id.to_string(),
        title: title.to_string(),
        updated_at_ms,
        size_bytes: 10,
    }
}

// --- the tree ----------------------------------------------------------------

#[tokio::test]
async fn the_tree_lists_every_category_in_render_order() {
    let fixture = Fixture::new(StorageDeps::unwired());
    let tree = fixture.service.tree().await.expect("tree");

    let ids: Vec<_> = tree.categories.iter().map(|c| c.id).collect();
    assert_eq!(
        ids,
        vec![
            StorageCategoryId::Models,
            StorageCategoryId::Binaries,
            StorageCategoryId::Cache,
            StorageCategoryId::Logs,
            StorageCategoryId::Sessions,
            StorageCategoryId::Extensions,
            StorageCategoryId::Settings,
        ],
        "the client draws its groups in this order, so it is contractual"
    );
    assert!(tree.categories.iter().all(|c| c.deletable));
}

#[tokio::test]
async fn a_category_size_is_the_sum_of_its_rows_and_the_total_is_the_sum_of_those() {
    let fixture = Fixture::with_models();
    fixture.model("alpha/repo/a.gguf", 40);
    fixture.model("beta/repo/b.gguf", 60);

    let tree = fixture.service.tree().await.expect("tree");
    let models = tree
        .categories
        .iter()
        .find(|c| c.id == StorageCategoryId::Models)
        .expect("models");
    assert_eq!(models.size, 100);
    assert_eq!(
        tree.total_size,
        tree.categories.iter().map(|c| c.size).sum::<i64>()
    );
}

/// The wired-and-unwired pair. Conformance cannot catch this: a freshly seeded
/// home has no models, so both answers look identical over the wire.
#[tokio::test]
async fn models_are_hidden_until_the_lock_source_is_wired() {
    let unwired = Fixture::new(StorageDeps::unwired());
    unwired.model("alpha/repo/a.gguf", 10);
    assert!(
        unwired
            .category(StorageCategoryId::Models)
            .await
            .nodes
            .is_empty(),
        "with no lock source every row would claim to be safe to delete"
    );
    // The rest of the view still builds, which is the property that makes
    // degrading acceptable at all.
    assert_eq!(
        unwired.category(StorageCategoryId::Cache).await.nodes.len(),
        3
    );

    let wired = Fixture::with_models();
    wired.model("alpha/repo/a.gguf", 10);
    assert_eq!(
        wired.category(StorageCategoryId::Models).await.nodes.len(),
        1
    );
}

#[tokio::test]
async fn the_cache_category_lists_the_three_directories_it_owns() {
    let fixture = Fixture::new(StorageDeps::unwired());
    fixture.write(&fixture.paths().cache_dir().join("blob"), 7);

    let cache = fixture.category(StorageCategoryId::Cache).await;
    let names: Vec<_> = cache.nodes.iter().map(node::node_name).collect();
    assert_eq!(names, ["cache", "deno-cache", "staging"]);
    assert_eq!(cache.size, 7);
}

#[tokio::test]
async fn the_logs_category_lists_files_only_and_sorts_them() {
    let fixture = Fixture::new(StorageDeps::unwired());
    fixture.write(&fixture.paths().log_file(), 5);
    fixture.write(&fixture.paths().logs_dir().join("core.log.1"), 3);
    std::fs::create_dir_all(fixture.paths().logs_dir().join("subdir")).expect("subdir");

    let logs = fixture.category(StorageCategoryId::Logs).await;
    let names: Vec<_> = logs.nodes.iter().map(node::node_name).collect();
    assert_eq!(names, ["core.log", "core.log.1"]);
    assert_eq!(logs.size, 8);
}

#[tokio::test]
async fn the_settings_row_sums_the_settings_file_and_the_secrets_bag() {
    let fixture = Fixture::new(StorageDeps::unwired());
    fixture.write(&fixture.paths().settings_file(), 30);
    fixture.write(&fixture.paths().secrets_enc_file(), 12);

    let settings = fixture.category(StorageCategoryId::Settings).await;
    assert_eq!(settings.nodes.len(), 1);
    assert_eq!(node::node_name(&settings.nodes[0]), "Settings + secrets");
    assert_eq!(settings.size, 42);
}

#[tokio::test]
async fn sessions_are_newest_first_and_an_active_one_is_locked() {
    let sessions = Arc::new(Sessions {
        listed: vec![
            session("older", "Older chat", 100),
            session("newer", "", 900),
            session("busy", "Busy chat", 500),
        ],
        active: HashSet::from(["busy".to_string()]),
        deleted: std::sync::Mutex::new(Vec::new()),
    });
    let fixture = Fixture::new(StorageDeps::unwired().with_sessions(sessions));

    let category = fixture.category(StorageCategoryId::Sessions).await;
    let names: Vec<_> = category.nodes.iter().map(node::node_name).collect();
    assert_eq!(
        names,
        ["Untitled session", "Busy chat", "Older chat"],
        "newest first, and an empty title falls back to a placeholder"
    );
    assert_eq!(
        node::node_lock_reason(&category.nodes[1]),
        Some("Session is active")
    );
}

/// `root_path` is the parent of the models directory, which equals the core
/// root only when the root was overridden. Every test here and the whole
/// conformance harness override it, so this is the one assertion that can see
/// the difference.
#[test]
fn root_path_is_the_models_parent_and_not_the_core_root() {
    let Ok(paths) = Paths::from_env(Channel::Stable) else {
        return; // a host with no home directory at all
    };
    assert_eq!(
        root_path(&paths),
        paths
            .models_dir()
            .parent()
            .expect("models parent")
            .to_string_lossy()
    );
    assert_ne!(
        root_path(&paths),
        paths.root().to_string_lossy(),
        "models live outside the channel root, so returning paths.root() here is wrong"
    );
}

// --- the delete guards -------------------------------------------------------

#[tokio::test]
async fn a_path_no_category_lists_is_skipped_rather_than_deleted() {
    let fixture = Fixture::new(StorageDeps::unwired());
    // Really inside the core root, and really not a row: the staging directory
    // is a node, its contents are not.
    let orphan = fixture.paths().staging_dir().join("orphan.bin");
    fixture.write(&orphan, 3);

    fixture
        .service
        .delete_paths(&[orphan.to_string_lossy().into_owned()])
        .await
        .expect("an unknown path is a no-op, not an error");

    // Not `exists()`: a cache-shaped delete removes the file and recreates a
    // directory at the same path, which `exists()` cannot tell from untouched.
    assert!(
        orphan.is_file() && walk::stat_size(&orphan) == 3,
        "only a path the core itself enumerated may be removed"
    );
}

#[tokio::test]
async fn traversal_targets_match_nothing_and_remove_nothing() {
    let fixture = Fixture::with_models();
    let model = fixture.model("alpha/repo/a.gguf", 10);
    let escapes = vec![
        // Climbs out of the models directory to the canary.
        format!(
            "{}/../../../outside/canary.txt",
            fixture.paths().models_dir().to_string_lossy()
        ),
        // Names it outright.
        fixture.canary().to_string_lossy().into_owned(),
        // A real row, spelled with a trailing separator, which is a different
        // key and so matches nothing.
        format!("{}/", model.to_string_lossy()),
        "/etc/passwd".to_string(),
        "../../../../etc/passwd".to_string(),
    ];

    fixture
        .service
        .delete_paths(&escapes)
        .await
        .expect("unmatched targets are skipped");

    fixture.assert_canary_intact();
    assert!(model.exists(), "no row was named, so nothing was removed");
}

#[tokio::test]
async fn a_locked_row_is_refused_and_the_request_removes_nothing() {
    let mut fixture = Fixture::new(StorageDeps::unwired());
    let locked = fixture.model("locked/repo/needed.gguf", 10);
    let spare = fixture.model("spare/repo/extra.gguf", 10);
    fixture.rewire(StorageDeps::unwired().with_models(Arc::new(Locks(vec![locked.clone()]))));

    let err = fixture
        .service
        .delete_paths(&[
            spare.to_string_lossy().into_owned(),
            locked.to_string_lossy().into_owned(),
        ])
        .await
        .expect_err("a locked row must be refused");
    assert_eq!(err.code, crate::error::ErrorCode::ValidationError);

    assert!(locked.exists(), "the locked row survives");
    assert!(
        spare.exists(),
        "and so does the unlocked one: every target is checked before anything is removed"
    );
    fixture.assert_canary_intact();
}

#[tokio::test]
async fn an_enumerated_row_is_removed() {
    let fixture = Fixture::with_models();
    let model = fixture.model("alpha/repo/a.gguf", 10);
    let neighbour = fixture.model("beta/repo/b.gguf", 10);

    fixture
        .service
        .delete_paths(&[model.to_string_lossy().into_owned()])
        .await
        .expect("delete");

    assert!(!model.exists(), "the named row is gone");
    assert!(neighbour.exists(), "and only that row");
    fixture.assert_canary_intact();
}

#[tokio::test]
async fn the_live_log_is_truncated_and_a_rotated_one_is_removed() {
    let fixture = Fixture::new(StorageDeps::unwired());
    let live = fixture.paths().log_file();
    let rotated = fixture.paths().logs_dir().join("core.log.1");
    fixture.write(&live, 500);
    fixture.write(&rotated, 500);

    fixture
        .service
        .clear_category("logs")
        .await
        .expect("clear logs");

    assert!(
        live.exists(),
        "removing the open log reclaims nothing on Unix and fails on Windows"
    );
    assert_eq!(walk::stat_size(&live), 0, "it is emptied in place");
    assert!(!rotated.exists(), "a rotated backup is removed outright");
}

#[tokio::test]
async fn clearing_the_cache_empties_the_directories_and_recreates_them() {
    let fixture = Fixture::new(StorageDeps::unwired());
    fixture.write(&fixture.paths().cache_dir().join("a/b.bin"), 100);
    fixture.write(&fixture.paths().deno_cache_dir().join("c.bin"), 100);

    fixture
        .service
        .clear_category("cache")
        .await
        .expect("clear cache");

    for dir in [
        fixture.paths().cache_dir(),
        fixture.paths().deno_cache_dir(),
        fixture.paths().staging_dir(),
    ] {
        assert!(dir.is_dir(), "{} must still exist", dir.display());
        assert_eq!(walk::dir_size(&dir), 0);
    }
    let cache = fixture.category(StorageCategoryId::Cache).await;
    assert_eq!(cache.size, 0);
}

#[tokio::test]
async fn clearing_a_category_skips_its_locked_rows() {
    let mut fixture = Fixture::new(StorageDeps::unwired());
    let locked = fixture.model("locked/repo/needed.gguf", 10);
    let spare = fixture.model("spare/repo/extra.gguf", 10);
    fixture.rewire(StorageDeps::unwired().with_models(Arc::new(Locks(vec![locked.clone()]))));

    fixture
        .service
        .clear_category("models")
        .await
        .expect("clear models");

    assert!(locked.exists(), "an in-use row is left alone");
    assert!(!spare.exists(), "everything else goes");
    fixture.assert_canary_intact();
}

#[tokio::test]
async fn clearing_the_sessions_category_goes_through_the_store() {
    let sessions = Arc::new(Sessions {
        listed: vec![session("kept", "Busy", 2), session("gone", "Idle", 1)],
        active: HashSet::from(["kept".to_string()]),
        deleted: std::sync::Mutex::new(Vec::new()),
    });
    let source: Arc<dyn SessionsSource> = Arc::clone(&sessions) as Arc<dyn SessionsSource>;
    let fixture = Fixture::new(StorageDeps::unwired().with_sessions(source));

    fixture
        .service
        .clear_category("sessions")
        .await
        .expect("clear sessions");

    let deleted = sessions.deleted.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        *deleted,
        vec!["gone".to_string()],
        "the store deletes by id, and the active session is skipped"
    );
}

#[tokio::test]
async fn an_unknown_category_is_a_validation_error() {
    let fixture = Fixture::new(StorageDeps::unwired());
    let err = fixture
        .service
        .clear_category("not-a-category")
        .await
        .expect_err("unknown category");
    assert_eq!(err.code, crate::error::ErrorCode::ValidationError);
}

/// The settings row exists on an unwired core, so its reclaim is reachable.
/// Reporting success without resetting anything would be a lie the user cannot
/// see, so the stub fails instead.
#[tokio::test]
async fn a_factory_reset_fails_loudly_when_the_settings_store_is_unwired() {
    let fixture = Fixture::new(StorageDeps::unwired());
    fixture.write(&fixture.paths().settings_file(), 10);
    let settings = fixture.category(StorageCategoryId::Settings).await;
    let path = node::node_path(&settings.nodes[0]).to_string();

    let err = fixture
        .service
        .delete_paths(&[path])
        .await
        .expect_err("an unwired factory reset must not report success");
    assert_eq!(err.code, crate::error::ErrorCode::InternalError);
    assert!(
        fixture.paths().settings_file().exists(),
        "and it must not have removed the file as a consolation"
    );
}
