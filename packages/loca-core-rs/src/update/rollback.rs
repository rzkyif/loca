//! Boot-time rollback: decide whether the running binary deserves to stay, and
//! put the previous one back when it does not. Runs before anything binds, so
//! the log and the `<root>/last-error.txt` breadcrumb are the only diagnosis.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::paths::Paths;
use crate::update::marker::{self, MarkerState, UpdateMarker};

/// Boots the new binary gets to reach its healthy checkpoint. Greater than one
/// so a single boot interrupted before the checkpoint is not read as a crash.
pub const MAX_BOOT_ATTEMPTS: u32 = 2;

/// Decide what to do about a pending update, before anything else boots.
///
/// Returns `true` when a rollback happened; the caller then exits NON-ZERO,
/// since systemd's `Restart=on-failure` would not relaunch on a clean exit.
pub async fn handle_update_marker_on_boot(config: &Config, paths: &Paths) -> bool {
    let marker = match marker::read(paths).await {
        MarkerState::Absent => return false,
        MarkerState::Unreadable(reason) => {
            // Left on disk as evidence; `commit_update` clears it on the first healthy boot.
            tracing::warn!(
                target: "loca::update.rollback",
                "an update marker is present but unreadable ({reason}); \
                 continuing to boot, and it will be cleared once this binary \
                 reaches its healthy checkpoint",
            );
            return false;
        }
        MarkerState::Present(marker) => marker,
    };

    if marker.version != config.version {
        if marker.previous_version == config.version {
            tracing::info!(
                target: "loca::update.rollback",
                "update marker present but we are already on previousVersion {}; clearing",
                config.version,
            );
        } else {
            tracing::warn!(
                target: "loca::update.rollback",
                "update marker version {} does not match running {} and is not \
                 previousVersion {}; clearing",
                marker.version,
                config.version,
                marker.previous_version,
            );
        }
        marker::delete(paths).await;
        return false;
    }

    if marker.attempts >= MAX_BOOT_ATTEMPTS {
        return perform_rollback(config, paths, &marker).await;
    }

    if let Err(err) = marker::bump(paths, &marker).await {
        // Not fatal: an unbumped marker retries rather than rolls back, which is the safe side.
        tracing::warn!(
            target: "loca::update.rollback",
            "could not record this boot attempt ({err}); the retry budget will \
             not advance and a failing binary will be retried rather than rolled back",
        );
    }
    tracing::info!(
        target: "loca::update.rollback",
        "update marker: boot {}/{MAX_BOOT_ATTEMPTS} of v{} (was v{}); commits once it boots healthy",
        marker.attempts + 1,
        marker.version,
        marker.previous_version,
    );
    false
}

/// Commit a pending update: drop the anchor and the marker.
///
/// Called once the HTTP listener is bound, so an ordinary restart of a working
/// binary can never trip a rollback. Idempotent; a no-op with nothing pending.
pub async fn commit_update(config: &Config, paths: &Paths) {
    if !exists(&paths.update_marker_file()).await {
        return;
    }
    let old_bin = old_binary_path(config, paths);
    match tokio::fs::remove_file(&old_bin).await {
        Ok(()) => tracing::info!(
            target: "loca::update.rollback",
            "update committed; removed rollback anchor {}",
            old_bin.display(),
        ),
        // Normal on a first install: a marker with no anchor behind it.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(
            target: "loca::update.rollback",
            "update committed, but the rollback anchor {} could not be removed: {err}",
            old_bin.display(),
        ),
    }
    marker::delete(paths).await;
}

/// Whether the anchor can be rolled onto. Shared with `swap.rs`, so the module
/// that creates an anchor and the one that reads it agree on "usable".
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Anchor {
    Missing,
    /// Present and provably unusable, with the reason for the log.
    Unusable(String),
    Usable {
        size: u64,
    },
}

/// Inspect the anchor without trusting its mere existence: an interrupted copy
/// can leave an empty file that passes `stat`, and rolling onto it replaces a
/// binary that might start with one that cannot. A truncated but non-empty
/// anchor is not detectable here; `swap.rs` checks the length at creation.
pub(crate) fn inspect_anchor(path: &Path) -> Anchor {
    let Ok(meta) = std::fs::metadata(path) else {
        return Anchor::Missing;
    };
    if !meta.is_file() {
        return Anchor::Unusable("it is not a regular file".into());
    }
    if meta.len() == 0 {
        return Anchor::Unusable("it is empty, so an interrupted copy left it behind".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o111 == 0 {
            return Anchor::Unusable("it is not executable".into());
        }
    }
    Anchor::Usable { size: meta.len() }
}

/// Put the previous binary back. Returns `true` only when it is now live.
async fn perform_rollback(config: &Config, paths: &Paths, marker: &UpdateMarker) -> bool {
    let old_bin = old_binary_path(config, paths);
    let size = match inspect_anchor(&old_bin) {
        Anchor::Usable { size } => size,
        Anchor::Missing => {
            return unrecoverable(
                paths,
                format!(
                    "Loca Core v{} could not start and the rollback anchor {} is missing, \
                     so the previous version cannot be restored. Reinstall Loca Core.",
                    marker.version,
                    old_bin.display(),
                ),
            )
            .await;
        }
        Anchor::Unusable(why) => {
            return unrecoverable(
                paths,
                format!(
                    "Loca Core v{} could not start, and the rollback anchor {} cannot be \
                     used because {why}. The previous version was not restored, because \
                     installing an unusable binary would leave nothing runnable at all. \
                     Reinstall Loca Core.",
                    marker.version,
                    old_bin.display(),
                ),
            )
            .await;
        }
    };

    let current_bin = paths.bin_path(&core_binary_name(config));
    let broken_bin = with_suffix(&current_bin, ".broken");
    let staged_old = with_suffix(&old_bin, ".staged");

    tracing::warn!(
        target: "loca::update.rollback",
        "rolling back v{} to v{}: the new binary failed to reach a healthy \
         checkpoint in {MAX_BOOT_ATTEMPTS} boots",
        marker.version,
        marker.previous_version,
    );

    // Leftovers from an earlier attempt; best-effort.
    let _ = tokio::fs::remove_file(&broken_bin).await;
    let _ = tokio::fs::remove_file(&staged_old).await;
    warn_if_disk_bound(paths, size).await;

    // Copy-first staging, so the original anchor survives until the install
    // rename has succeeded:
    //   1. copy   oldBin     -> stagedOld
    //   2. rename currentBin -> brokenBin   (aside; atomic on one filesystem)
    //   3. rename stagedOld  -> currentBin  (install)
    //   4. remove oldBin                    (committed)
    let used_copy = match tokio::fs::copy(&old_bin, &staged_old).await {
        Ok(_) => true,
        Err(err) => {
            // Degraded two-rename path rather than aborting: a disk too full to
            // copy is exactly when a rollback is most needed.
            tracing::warn!(
                target: "loca::update.rollback",
                "copy-first staging failed ({err}); falling back to the two-rename swap, \
                 which cannot restore the current binary if the install rename fails",
            );
            false
        }
    };

    if let Err(err) = tokio::fs::rename(&current_bin, &broken_bin).await {
        if used_copy {
            let _ = tokio::fs::remove_file(&staged_old).await;
        }
        tracing::error!(target: "loca::update.rollback", "rollback aside failed: {err}");
        // Nothing moved, so the broken binary is still launchable; least-bad failure.
        return unrecoverable(
            paths,
            format!(
                "Loca Core v{} could not start, and rolling back to v{} failed while \
                 moving the current binary aside ({err}). Reinstall Loca Core.",
                marker.version, marker.previous_version,
            ),
        )
        .await;
    }

    let install_src = if used_copy { &staged_old } else { &old_bin };
    if let Err(err) = install_rename(install_src, &current_bin).await {
        tracing::error!(
            target: "loca::update.rollback",
            "rollback install failed: {err}; restoring the broken binary so the \
             supervisor has something to run",
        );
        let restored = tokio::fs::rename(&broken_bin, &current_bin).await;
        if let Err(revert_err) = &restored {
            tracing::error!(
                target: "loca::update.rollback",
                "revert of the aside also failed: {revert_err}. There is now no binary at {}. \
                 Manual reinstall required.",
                current_bin.display(),
            );
        }
        if used_copy {
            let _ = tokio::fs::remove_file(&staged_old).await;
        }
        let tail = if restored.is_ok() {
            "The previous version was not installed and the current binary was put back."
        } else {
            "There is now no binary at the install path."
        };
        return unrecoverable(
            paths,
            format!(
                "Loca Core v{} could not start, and rolling back to v{} failed while \
                 installing the previous binary ({err}). {tail} Reinstall Loca Core.",
                marker.version, marker.previous_version,
            ),
        )
        .await;
    }

    set_executable(&current_bin).await;
    if used_copy {
        let _ = tokio::fs::remove_file(&old_bin).await;
    }
    marker::delete(paths).await;
    // `broken_bin` is kept on disk: it is the only artefact of the failed release.
    tracing::info!(
        target: "loca::update.rollback",
        "rolled back to v{}; exiting so the supervisor relaunches it. The binary that \
         failed is kept at {} for diagnosis.",
        marker.previous_version,
        broken_bin.display(),
    );
    true
}

/// Log the reason, leave it where the Client can read it, clear the marker so
/// the impossible recovery is not retried every launch. Always returns `false`.
async fn unrecoverable(paths: &Paths, message: String) -> bool {
    tracing::error!(target: "loca::update.rollback", "{message}");
    let scrubbed = crate::log::scrub_secrets(&message);
    if let Err(err) = crate::files::write_atomic(
        paths.boot_error_file(),
        format!("{scrubbed}\n").into_bytes(),
    )
    .await
    {
        tracing::warn!(
            target: "loca::update.rollback",
            "could not write the boot-failure breadcrumb: {err}",
        );
    }
    marker::delete(paths).await;
    false
}

/// Informational only: names the likely cause before a "copy failed" line.
async fn warn_if_disk_bound(paths: &Paths, needed: u64) {
    let Some(free) = free_bytes(&paths.bin_dir()) else {
        return;
    };
    if free < needed.saturating_mul(2) {
        tracing::warn!(
            target: "loca::update.rollback",
            "low free space (need about {needed} bytes for a safe swap, about {free} \
             bytes free in the bin dir); the rollback will fall back if the copy fails",
        );
    }
}

#[cfg(unix)]
fn free_bytes(dir: &Path) -> Option<u64> {
    let stats = nix::sys::statvfs::statvfs(dir).ok()?;
    Some(stats.blocks_available() as u64 * stats.fragment_size() as u64)
}

#[cfg(not(unix))]
fn free_bytes(_dir: &Path) -> Option<u64> {
    None
}

async fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(err) =
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).await
        {
            tracing::warn!(
                target: "loca::update.rollback",
                "restored binary {} could not be marked executable: {err}",
                path.display(),
            );
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Step 3 of the swap, behind a test-only fault injector: once step 2 has
/// vacated the destination nothing a test can stage makes this rename fail.
async fn install_rename(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if crate::update::testing::install_rename_should_fail() {
        return Err(std::io::Error::other(
            "d13: injected install-rename failure",
        ));
    }
    tokio::fs::rename(src, dst).await
}

async fn exists(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// `<bin>.old`, channel-suffixed so a latest install rolls back its own binary.
fn old_binary_path(config: &Config, paths: &Paths) -> PathBuf {
    with_suffix(&paths.bin_path(&core_binary_name(config)), ".old")
}

fn core_binary_name(config: &Config) -> String {
    config.channel.core_binary_name("loca-core")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::{Channel, CORE_VERSION};
    use crate::update::testing;

    /// A core root with a bin dir. Holds the shared rollback guard for its
    /// whole life, since the install-rename injector is process-global.
    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        config: Config,
        _guard: crate::update::testing::RollbackGuard,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            testing::set_install_rename_failure(false);
        }
    }

    impl Fixture {
        async fn new() -> Self {
            Self::on_channel(Channel::Stable).await
        }

        /// "new" live, "old" anchored, marker for this version at `attempts`.
        async fn pending(attempts: u32) -> Self {
            let f = Self::new().await;
            f.place(&f.current(), "new");
            f.place(&f.anchor(), "old");
            f.stage_marker(CORE_VERSION, "0.0.1", attempts).await;
            f
        }

        fn fail_install_rename(&self) {
            testing::set_install_rename_failure(true);
        }

        async fn on_channel(channel: Channel) -> Self {
            let guard = testing::rollback_guard().await;
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::rooted(dir.path().join("core"));
            paths.ensure_dirs().unwrap();
            std::fs::create_dir_all(paths.bin_dir()).unwrap();
            let config = Config {
                channel,
                host: "127.0.0.1".into(),
                port: 7800,
                version: CORE_VERSION,
                storage_base_url: String::new(),
                hf_base_url: String::new(),
            };
            Self {
                _dir: dir,
                paths,
                config,
                _guard: guard,
            }
        }

        fn current(&self) -> PathBuf {
            self.paths.bin_path(&core_binary_name(&self.config))
        }

        fn anchor(&self) -> PathBuf {
            old_binary_path(&self.config, &self.paths)
        }

        /// Executable with recognisable contents, so a test can tell WHICH
        /// binary ended up live.
        fn place(&self, path: &Path, contents: &str) {
            std::fs::write(path, contents).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        fn live_contents(&self) -> String {
            std::fs::read_to_string(self.current()).unwrap()
        }

        async fn breadcrumb(&self) -> Option<String> {
            tokio::fs::read_to_string(self.paths.boot_error_file())
                .await
                .ok()
        }

        async fn stage_marker(&self, version: &str, previous: &str, attempts: u32) {
            let marker = UpdateMarker {
                version: version.into(),
                previous_version: previous.into(),
                attempts,
            };
            crate::files::write_json_atomic(self.paths.update_marker_file(), &marker)
                .await
                .unwrap();
        }

        async fn boot(&self) -> bool {
            handle_update_marker_on_boot(&self.config, &self.paths).await
        }

        async fn marker_absent(&self) -> bool {
            matches!(marker::read(&self.paths).await, MarkerState::Absent)
        }
    }

    #[tokio::test]
    async fn no_marker_is_a_silent_no_op() {
        let f = Fixture::new().await;
        assert!(!f.boot().await);
    }

    // The budget is pinned with LITERAL attempt counts, never MAX_BOOT_ATTEMPTS,
    // so halving the constant cannot scale the fixture with it.

    #[tokio::test]
    async fn one_interrupted_boot_is_retried_rather_than_rolled_back() {
        let f = Fixture::pending(0).await;

        assert!(!f.boot().await);
        assert_eq!(f.live_contents(), "new", "nothing was swapped");

        let MarkerState::Present(marker) = marker::read(&f.paths).await else {
            panic!("d13: the marker was cleared on the first boot");
        };
        assert_eq!(marker.attempts, 1, "the attempt was recorded");
    }

    #[tokio::test]
    async fn a_second_interrupted_boot_is_still_retried() {
        let f = Fixture::pending(1).await;

        assert!(
            !f.boot().await,
            "the second boot spends the budget rather than rolling back"
        );
        assert_eq!(f.live_contents(), "new", "nothing was swapped");

        let MarkerState::Present(marker) = marker::read(&f.paths).await else {
            panic!("d13: the marker was cleared on the second boot");
        };
        assert_eq!(marker.attempts, 2);
    }

    #[tokio::test]
    async fn the_boot_after_the_budget_is_spent_rolls_back() {
        let f = Fixture::pending(2).await;

        assert!(f.boot().await);
        assert_eq!(f.live_contents(), "old", "the previous binary is live");
        assert!(f.marker_absent().await);
        assert!(
            with_suffix(&f.current(), ".broken").exists(),
            "the binary that failed is kept for diagnosis"
        );
        assert!(
            !f.anchor().exists(),
            "the anchor is consumed once its copy is live"
        );
    }

    #[tokio::test]
    async fn an_unrelated_marker_is_cleared_rather_than_acted_on() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "old");
        f.stage_marker("9.9.9", "9.9.8", 2).await;

        assert!(!f.boot().await);
        assert_eq!(f.live_contents(), "new");
        assert!(f.marker_absent().await);
    }

    #[tokio::test]
    async fn a_marker_for_the_version_we_already_rolled_back_to_is_cleared() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.stage_marker("9.9.9", CORE_VERSION, 2).await;

        assert!(!f.boot().await);
        assert!(f.marker_absent().await);
    }

    #[tokio::test]
    async fn an_unreadable_marker_continues_the_boot_and_is_left_for_the_commit() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        tokio::fs::write(f.paths.update_marker_file(), "{ not json")
            .await
            .unwrap();

        assert!(!f.boot().await);
        assert!(
            matches!(marker::read(&f.paths).await, MarkerState::Unreadable(_)),
            "left on disk, because commit_update clears it on the first healthy boot"
        );
    }

    #[tokio::test]
    async fn a_missing_anchor_leaves_the_current_binary_alone_and_says_why() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(!f.boot().await);
        assert_eq!(f.live_contents(), "new", "nothing was destroyed");
        assert!(
            f.marker_absent().await,
            "cleared, so the impossible recovery is not retried every launch"
        );
        let crumb = f.breadcrumb().await.expect("a breadcrumb was written");
        assert!(crumb.contains("Reinstall"), "{crumb}");
    }

    #[tokio::test]
    async fn an_empty_anchor_is_refused_rather_than_installed() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "");
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(!f.boot().await);
        assert_eq!(
            f.live_contents(),
            "new",
            "the binary that might still start is left in place"
        );
        let crumb = f.breadcrumb().await.expect("a breadcrumb was written");
        assert!(crumb.contains("empty"), "the reason is named: {crumb}");
    }

    #[tokio::test]
    async fn an_anchor_that_is_a_directory_is_refused() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        std::fs::create_dir_all(f.anchor()).unwrap();
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(!f.boot().await);
        assert_eq!(f.live_contents(), "new");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_executable_anchor_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        std::fs::write(f.anchor(), "old").unwrap();
        std::fs::set_permissions(f.anchor(), std::fs::Permissions::from_mode(0o644)).unwrap();
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(!f.boot().await);
        assert_eq!(f.live_contents(), "new");
        let crumb = f.breadcrumb().await.expect("a breadcrumb was written");
        assert!(crumb.contains("executable"), "{crumb}");
    }

    #[test]
    fn a_usable_anchor_is_the_only_state_that_permits_a_swap() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good");
        std::fs::write(&good, "binary").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&good, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(inspect_anchor(&good), Anchor::Usable { size: 6 });
        assert_eq!(inspect_anchor(&dir.path().join("nope")), Anchor::Missing);
        assert!(matches!(inspect_anchor(dir.path()), Anchor::Unusable(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_restored_binary_is_executable() {
        // End state only: copy and rename preserve the mode here, so this
        // passes with or without the `set_executable` call, which is kept for
        // filesystems that do not preserve permissions across a copy.
        use std::os::unix::fs::PermissionsExt as _;
        let f = Fixture::pending(2).await;

        assert!(f.boot().await);
        let mode = std::fs::metadata(f.current()).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "mode was {mode:o}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_aside_leaves_the_current_binary_where_the_supervisor_expects_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let f = Fixture::pending(2).await;
        // A read-only bin dir fails the rename without making the anchor unreadable.
        let bin_dir = f.paths.bin_dir();
        std::fs::set_permissions(&bin_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

        let rolled_back = f.boot().await;

        std::fs::set_permissions(&bin_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!rolled_back);
        assert_eq!(f.live_contents(), "new", "something is still runnable");
        assert!(f.marker_absent().await);
        assert!(f.breadcrumb().await.is_some());
    }

    #[tokio::test]
    async fn a_failed_install_puts_the_aside_binary_back() {
        let f = Fixture::pending(2).await;
        f.fail_install_rename();

        assert!(!f.boot().await);
        assert_eq!(
            f.live_contents(),
            "new",
            "the broken binary is put back, because something runnable beats nothing"
        );
        assert!(
            !with_suffix(&f.anchor(), ".staged").exists(),
            "the staging copy is cleaned up rather than left to confuse the next attempt"
        );
        assert!(f.marker_absent().await);
        let crumb = f.breadcrumb().await.expect("a breadcrumb was written");
        assert!(crumb.contains("put back"), "{crumb}");
    }

    #[tokio::test]
    async fn a_failed_copy_falls_back_to_the_two_rename_swap() {
        // A non-empty directory at the staging path is what `copy` cannot write over.
        let f = Fixture::pending(2).await;
        let staged = with_suffix(&f.anchor(), ".staged");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("occupied"), "x").unwrap();

        assert!(f.boot().await);
        assert_eq!(f.live_contents(), "old", "the rollback still happened");
    }

    #[tokio::test]
    async fn a_rollback_on_a_non_stable_channel_uses_that_channels_binary() {
        let f = Fixture::on_channel(Channel::Latest).await;
        assert!(f.current().to_string_lossy().contains("loca-core-latest"));
        f.place(&f.current(), "new-latest");
        f.place(&f.anchor(), "old-latest");
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(f.boot().await);
        assert_eq!(f.live_contents(), "old-latest");
    }

    #[tokio::test]
    async fn commit_removes_both_the_marker_and_the_anchor() {
        let f = Fixture::pending(1).await;

        commit_update(&f.config, &f.paths).await;

        assert!(f.marker_absent().await);
        assert!(!f.anchor().exists(), "the anchor is gone");
        assert_eq!(f.live_contents(), "new");
    }

    #[tokio::test]
    async fn commit_is_a_no_op_with_no_update_pending() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "old");

        commit_update(&f.config, &f.paths).await;
        assert!(
            f.anchor().exists(),
            "no marker means no pending update, so nothing to commit"
        );
    }

    #[tokio::test]
    async fn commit_clears_an_unreadable_marker() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        tokio::fs::write(f.paths.update_marker_file(), "{ not json")
            .await
            .unwrap();

        commit_update(&f.config, &f.paths).await;
        assert!(f.marker_absent().await);
    }

    #[tokio::test]
    async fn commit_tolerates_a_first_install_with_no_anchor() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.stage_marker(CORE_VERSION, "0.0.1", 1).await;

        commit_update(&f.config, &f.paths).await;
        assert!(f.marker_absent().await);
    }

    #[tokio::test]
    async fn a_committed_update_cannot_be_rolled_back_by_a_later_restart() {
        let f = Fixture::pending(0).await;

        assert!(!f.boot().await);
        commit_update(&f.config, &f.paths).await;

        for _ in 0..2 {
            assert!(!f.boot().await);
            assert_eq!(f.live_contents(), "new");
        }
    }
}
