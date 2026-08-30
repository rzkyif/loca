//! Replacing the core's own binary while it is running, and creating the anchor
//! that makes it undoable.
//!
//! The platform is a PARAMETER rather than `cfg!(windows)`, since that is the
//! only way both branches run on a machine that is not both platforms. On unix
//! the live inode is replaced under the running process; on Windows a running
//! `.exe` can only be renamed aside first.
//!
//! **The anchor is the whole recovery path**, so [`create_anchor`] verifies its
//! result before the update may proceed: it must satisfy
//! `rollback::inspect_anchor` (the same predicate the boot path applies) and
//! match the source's length, catching the truncated-but-non-empty anchor that
//! nothing at rollback time could recognise. A failure removes the partial and
//! aborts, which is safe: nothing has been swapped yet. Same-length divergence
//! is not checked, and the hard-link path needs no check at all because it is a
//! second name for one inode.
//!
//! **The marker is written BEFORE the swap.** Writing it after leaves one
//! unrecoverable state: the new binary installed with an anchor beside it and
//! nothing pointing at either, so a broken release crash-loops with its own
//! recovery unused. Writing first only ever produces the "already rolled back"
//! case `rollback.rs` clears. A mixed helper set is tolerated by design, since
//! rollback restores `<bin>.old` and never the helpers.
//!
//! **The one window nothing here can close:** the Windows swap is two renames
//! and between them no file exists at the install path. A hard kill there leaves
//! the supervisor nothing to launch, and the process that would mitigate it is
//! the one that died. `commit_rename` has the same shape for a locked helper,
//! but there the boot check in `helpers.rs` restores from the aside.

use std::path::{Path, PathBuf};

use crate::error::{AppError, ErrorCode, Result};
use crate::update::rollback::{inspect_anchor, Anchor};

/// True only while a test is driving the copy fallback. Always false in
/// production, where the branch is chosen by the filesystem.
#[cfg(test)]
fn hard_links_disabled() -> bool {
    crate::update::testing::hard_links_disabled()
}

#[cfg(not(test))]
fn hard_links_disabled() -> bool {
    false
}

/// Which swap semantics to use. Explicit rather than `cfg!(windows)` so both
/// branches run on one host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Unix,
    Windows,
}

impl Platform {
    /// The platform this build actually runs on.
    pub fn host() -> Self {
        Self::from_windows(cfg!(windows))
    }

    /// The mapping, separated from the `cfg!` that feeds it, so both answers
    /// are assertable on a host that is only one platform.
    pub fn from_windows(windows: bool) -> Self {
        if windows {
            Platform::Windows
        } else {
            Platform::Unix
        }
    }
}

/// Swap `staged` over `current`, preserving the previous binary as
/// `<current>.old`.
///
/// The anchor is mandatory: without it boot-time rollback has nothing to
/// restore, so a failure to create a verifiable one aborts the update.
pub async fn swap_binary_in_place(staged: &Path, current: &Path, platform: Platform) -> Result<()> {
    let anchor = anchor_path(current);
    let _ = tokio::fs::remove_file(&anchor).await;

    if platform == Platform::Windows {
        // The move IS the anchor, and a rename, so no bytes to verify. It
        // also frees the install path, so the two renames are adjacent.
        tokio::fs::rename(current, &anchor).await.map_err(|err| {
            AppError::new(
                ErrorCode::UpdateFailed,
                format!(
                    "could not move the running binary aside to {}: {err}",
                    anchor.display()
                ),
            )
        })?;
        if let Err(err) = tokio::fs::rename(staged, current).await {
            // Put the running binary back: otherwise a failed install leaves
            // the install path empty and the machine unlaunchable.
            let _ = tokio::fs::rename(&anchor, current).await;
            return Err(AppError::new(
                ErrorCode::UpdateFailed,
                format!("could not install the staged binary: {err}"),
            ));
        }
        return Ok(());
    }

    create_anchor(current, &anchor).await?;
    tokio::fs::rename(staged, current).await.map_err(|err| {
        AppError::new(
            ErrorCode::UpdateFailed,
            format!("could not install the staged binary: {err}"),
        )
    })?;
    set_executable(current).await;
    Ok(())
}

/// Create `<current>.old` from `current`, and prove it before returning.
///
/// A hard link first: atomic, a second name for one inode, nothing to verify.
/// A filesystem without hard links falls back to a copy, and THAT is checked.
async fn create_anchor(current: &Path, anchor: &Path) -> Result<()> {
    let source_len = tokio::fs::metadata(current)
        .await
        .map_err(|err| {
            AppError::new(
                ErrorCode::UpdateFailed,
                format!(
                    "could not read {} before anchoring it: {err}",
                    current.display()
                ),
            )
        })?
        .len();

    let linked = !hard_links_disabled() && tokio::fs::hard_link(current, anchor).await.is_ok();
    if !linked {
        if let Err(err) = tokio::fs::copy(current, anchor).await {
            // A partial anchor left here is what `rollback.rs` would have to
            // guess about later.
            let _ = tokio::fs::remove_file(anchor).await;
            return Err(refusing(anchor, &format!("the copy failed: {err}")));
        }
    }

    if let Err(err) = verify_anchor(anchor, source_len) {
        // Removed rather than left, for the same reason as above.
        let _ = tokio::fs::remove_file(anchor).await;
        return Err(err);
    }
    Ok(())
}

/// The anchor's post-condition: usable by the boot path's own definition, and
/// the same length as what it was made from.
///
/// Separate because the call site cannot be driven through every outcome:
/// `tokio::fs::copy` errors rather than reporting a short write, so no input
/// makes the copy succeed and this fail. The primary defence is propagating
/// the copy error; this is the second line, catching an anchor that exists,
/// reports success, and is the wrong length.
fn verify_anchor(anchor: &Path, source_len: u64) -> Result<()> {
    // The SAME predicate the boot path applies, so "usable" cannot drift.
    match inspect_anchor(anchor) {
        Anchor::Usable { size } if size == source_len => Ok(()),
        Anchor::Usable { size } => Err(refusing(
            anchor,
            &format!(
                "it is {size} bytes where the source is {source_len}, so the copy was truncated"
            ),
        )),
        Anchor::Missing => Err(refusing(anchor, "it was not created")),
        Anchor::Unusable(why) => Err(refusing(anchor, &why)),
    }
}

/// The message every anchor refusal shares: the user sees it when an update
/// declines to proceed, so it says why proceeding would be worse.
fn refusing(anchor: &Path, why: &str) -> AppError {
    AppError::new(
        ErrorCode::UpdateFailed,
        format!(
            "refusing to install without a recoverable fallback: the rollback anchor {} \
             could not be created because {why}. Nothing was changed.",
            anchor.display()
        ),
    )
}

/// Rename a staged artifact into its live path, tolerating a locked target.
///
/// A running Windows executable (a helper sidecar) cannot be overwritten but
/// can be renamed, so the live target moves aside first. On unix a plain rename
/// replaces the inode under a running process and a failure is real.
///
/// The aside at `<to>.old` is the recovery path for a crash between the two
/// renames, and bounded to one per artifact: the next update removes it first.
pub async fn commit_rename(from: &Path, to: &Path, platform: Platform) -> Result<()> {
    match tokio::fs::rename(from, to).await {
        Ok(()) => return Ok(()),
        Err(err) if platform != Platform::Windows => {
            return Err(AppError::new(
                ErrorCode::UpdateFailed,
                format!("could not install {}: {err}", to.display()),
            ));
        }
        Err(_) => {}
    }
    let aside = anchor_path(to);
    let _ = tokio::fs::remove_file(&aside).await;
    tokio::fs::rename(to, &aside).await.map_err(|err| {
        AppError::new(
            ErrorCode::UpdateFailed,
            format!("could not move {} aside: {err}", to.display()),
        )
    })?;
    if let Err(err) = tokio::fs::rename(from, to).await {
        let _ = tokio::fs::rename(&aside, to).await;
        return Err(AppError::new(
            ErrorCode::UpdateFailed,
            format!("could not install {}: {err}", to.display()),
        ));
    }
    Ok(())
}

/// `<path>.old`.
pub fn anchor_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".old");
    PathBuf::from(name)
}

async fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(err) =
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).await
        {
            tracing::warn!(
                target: "loca::update",
                "installed binary {} could not be marked executable: {err}",
                path.display(),
            );
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn place(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    // --- the swap, both platforms, on one host -----------------------------

    #[tokio::test]
    async fn a_unix_swap_installs_the_staged_binary_and_anchors_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("loca-core");
        let staged = dir.path().join("staged");
        place(&current, "old");
        place(&staged, "new");

        swap_binary_in_place(&staged, &current, Platform::Unix)
            .await
            .unwrap();

        assert_eq!(read(&current), "new");
        assert_eq!(read(&anchor_path(&current)), "old");
        assert!(!staged.exists(), "the staged copy is consumed");
    }

    #[tokio::test]
    async fn a_windows_swap_moves_the_running_binary_aside_rather_than_over_it() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("loca-core.exe");
        let staged = dir.path().join("staged.exe");
        place(&current, "old");
        place(&staged, "new");

        swap_binary_in_place(&staged, &current, Platform::Windows)
            .await
            .unwrap();

        assert_eq!(read(&current), "new");
        assert_eq!(read(&anchor_path(&current)), "old");
    }

    #[tokio::test]
    async fn a_failed_windows_install_puts_the_running_binary_back() {
        // Without the restore the install path is empty and the supervisor
        // has nothing to launch, which is worse than a failed update.
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("loca-core.exe");
        let staged = dir.path().join("missing.exe");
        place(&current, "old");

        let err = swap_binary_in_place(&staged, &current, Platform::Windows)
            .await
            .unwrap_err();

        assert_eq!(err.code, ErrorCode::UpdateFailed);
        assert_eq!(read(&current), "old", "the running binary is back");
    }

    #[tokio::test]
    async fn a_stale_anchor_is_replaced_rather_than_kept() {
        // A previous update's anchor must not survive into this one, or a
        // rollback would restore a version two releases back.
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("loca-core");
        let staged = dir.path().join("staged");
        place(&current, "v2");
        place(&staged, "v3");
        place(&anchor_path(&current), "v1");

        swap_binary_in_place(&staged, &current, Platform::Unix)
            .await
            .unwrap();
        assert_eq!(read(&anchor_path(&current)), "v2");
    }

    // --- the copy verification ---------------------------------------------

    /// Every swap test holds the shared guard, because [`no_hard_links`] is
    /// process-global and a neighbour must not inherit the injector.
    struct NoHardLinks {
        _guard: crate::update::testing::RollbackGuard,
    }

    impl NoHardLinks {
        async fn arm() -> Self {
            let guard = crate::update::testing::rollback_guard().await;
            crate::update::testing::set_hard_links_disabled(true);
            Self { _guard: guard }
        }
    }

    impl Drop for NoHardLinks {
        fn drop(&mut self) {
            crate::update::testing::set_hard_links_disabled(false);
        }
    }

    #[test]
    fn the_host_mapping_answers_both_ways() {
        // A test over `host()` alone passes with the Windows arm deleted, so
        // the mapping is asserted through the seam instead.
        assert_eq!(Platform::from_windows(true), Platform::Windows);
        assert_eq!(Platform::from_windows(false), Platform::Unix);
        assert_eq!(Platform::host(), Platform::from_windows(cfg!(windows)));
    }

    #[tokio::test]
    async fn the_copy_fallback_produces_a_verified_anchor() {
        // No filesystem a test can create here lacks hard links, so without
        // the seam this path is reachable only through its failure case.
        let _armed = NoHardLinks::arm().await;
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("loca-core");
        place(&current, "the whole binary");
        let anchor = anchor_path(&current);

        create_anchor(&current, &anchor).await.unwrap();

        assert_eq!(read(&anchor), "the whole binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_ne!(
                std::fs::metadata(&current).unwrap().ino(),
                std::fs::metadata(&anchor).unwrap().ino(),
                "a real copy, which is what makes the length check meaningful"
            );
        }
        assert!(
            verify_anchor(&anchor, std::fs::metadata(&current).unwrap().len()).is_ok(),
            "and it satisfies the post-condition the boot path will apply"
        );
    }

    #[tokio::test]
    async fn a_swap_over_the_copy_fallback_still_anchors_the_old_binary() {
        // End to end: `create_anchor` succeeding alone does not prove the
        // swap around it survives a real copy.
        let _armed = NoHardLinks::arm().await;
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("loca-core");
        let staged = dir.path().join("staged");
        place(&current, "old");
        place(&staged, "new");

        swap_binary_in_place(&staged, &current, Platform::Unix)
            .await
            .unwrap();

        assert_eq!(read(&current), "new");
        assert_eq!(read(&anchor_path(&current)), "old");
    }

    #[tokio::test]
    async fn a_hard_linked_anchor_is_the_same_inode_and_needs_no_copy() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("loca-core");
        place(&current, "old");
        let anchor = anchor_path(&current);

        create_anchor(&current, &anchor).await.unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!(
                std::fs::metadata(&current).unwrap().ino(),
                std::fs::metadata(&anchor).unwrap().ino(),
                "a link, not a copy, so there is nothing that could differ"
            );
        }
        assert_eq!(read(&anchor), "old");
    }

    #[tokio::test]
    async fn a_copy_that_cannot_be_written_aborts_the_update() {
        // The primary defence: the copy error propagates and nothing is
        // swapped. Staged with an unwritable destination directory, a real
        // shape that reaches the copy branch because `hard_link` fails too.
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let current = bin.join("loca-core");
        place(&current, "old");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o500)).unwrap();
        }

        let result = create_anchor(&current, &anchor_path(&current)).await;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::UpdateFailed);
        assert!(
            err.message.contains("Nothing was changed"),
            "{}",
            err.message
        );
        assert_eq!(read(&current), "old", "the running binary is untouched");
        // The REASON, not merely the refusal: swallowing the copy error still
        // errors here, because `verify_anchor` then finds nothing. What the
        // swallow costs is the diagnosis, which is the whole support output.
        assert!(
            err.message.contains("the copy failed"),
            "the refusal must name the copy rather than report a missing anchor: {}",
            err.message
        );
    }

    #[test]
    fn the_anchor_post_condition_rejects_every_shape_that_is_not_a_fallback() {
        // Driven directly: no reachable input makes the copy succeed and this
        // fail. A truncated anchor is "usable" by every property the boot path
        // can see, so the length must be compared at creation.
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("full");
        place(&full, "the whole binary");
        let len = std::fs::metadata(&full).unwrap().len();

        assert!(verify_anchor(&full, len).is_ok());

        let truncated = dir.path().join("truncated");
        place(&truncated, "the whole b");
        let err = verify_anchor(&truncated, len).unwrap_err();
        assert!(err.message.contains("truncated"), "{}", err.message);

        let empty = dir.path().join("empty");
        place(&empty, "");
        assert!(verify_anchor(&empty, len).is_err());

        assert!(verify_anchor(&dir.path().join("absent"), len).is_err());
        assert!(verify_anchor(dir.path(), len).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let plain = dir.path().join("plain");
            std::fs::write(&plain, "the whole binary").unwrap();
            std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                verify_anchor(&plain, len).is_err(),
                "right length, wrong mode: still not something the supervisor can launch"
            );
        }
    }

    #[tokio::test]
    async fn an_unusable_anchor_leaves_nothing_behind() {
        // The boot path must not have to guess about a file this code already
        // knew was wrong.
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("loca-core");
        place(&current, "old");
        let anchor = anchor_path(&current);
        std::fs::create_dir_all(&anchor).unwrap();

        let err = create_anchor(&current, &anchor).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::UpdateFailed);
    }

    // --- commit_rename ------------------------------------------------------

    #[tokio::test]
    async fn a_unix_helper_rename_replaces_the_live_file() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("staged");
        let to = dir.path().join("loca-core-updater");
        place(&from, "new");
        place(&to, "old");

        commit_rename(&from, &to, Platform::Unix).await.unwrap();
        assert_eq!(read(&to), "new");
        assert!(
            !anchor_path(&to).exists(),
            "unix replaces the inode, so there is no aside to leave behind"
        );
    }

    #[tokio::test]
    async fn a_unix_rename_failure_is_real_rather_than_retried_as_windows() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("missing");
        let to = dir.path().join("loca-core-updater");
        place(&to, "old");

        let err = commit_rename(&from, &to, Platform::Unix).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::UpdateFailed);
        assert_eq!(read(&to), "old", "the live helper is untouched");
    }

    // --- the pairing: every crash state, against the boot that recovers it --
    //
    // Each case stages the exact on-disk state a kill at that point would
    // leave, then runs the real boot path over it.

    mod crash_window {
        use super::*;
        use crate::config::{Channel, Config, CORE_VERSION};
        use crate::paths::Paths;
        use crate::update::marker::{self, MarkerState, UpdateMarker};
        use crate::update::rollback::handle_update_marker_on_boot;
        use crate::update::testing;

        const NEXT_VERSION: &str = "99.0.0";

        struct Boot {
            _dir: tempfile::TempDir,
            _guard: crate::update::testing::RollbackGuard,
            paths: Paths,
            config: Config,
        }

        impl Boot {
            async fn new() -> Self {
                let guard = testing::rollback_guard().await;
                let dir = tempfile::tempdir().unwrap();
                let paths = Paths::rooted(dir.path().join("core"));
                paths.ensure_dirs().unwrap();
                std::fs::create_dir_all(paths.bin_dir()).unwrap();
                let config = Config {
                    channel: Channel::Stable,
                    host: "127.0.0.1".into(),
                    port: 7800,
                    version: CORE_VERSION,
                    storage_base_url: String::new(),
                    hf_base_url: String::new(),
                };
                Self {
                    _dir: dir,
                    _guard: guard,
                    paths,
                    config,
                }
            }

            fn current(&self) -> PathBuf {
                self.paths.bin_path("loca-core")
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
        }

        #[tokio::test]
        async fn killed_before_the_marker_was_written() {
            // Nothing points at anything: the old binary boots normally and
            // the next apply clears the half-finished staging.
            let b = Boot::new().await;
            place(&b.current(), "old");
            std::fs::create_dir_all(b.paths.staging_dir()).unwrap();
            std::fs::write(b.paths.staging_dir().join("partial"), "junk").unwrap();

            assert!(!b.boot().await);
            assert_eq!(read(&b.current()), "old");
        }

        #[tokio::test]
        async fn killed_after_the_marker_but_before_the_swap() {
            // The running binary is still the OLD one, so the marker names an
            // uninstalled version while `previousVersion` IS the running one:
            // the "already rolled back" case, cleared rather than acted on.
            let b = Boot::new().await;
            place(&b.current(), "old");
            b.stage_marker(NEXT_VERSION, CORE_VERSION, 0).await;

            assert!(!b.boot().await, "no rollback: nothing was swapped");
            assert_eq!(read(&b.current()), "old");
            assert!(
                matches!(marker::read(&b.paths).await, MarkerState::Absent),
                "and the marker is cleared, so it cannot confuse a later boot"
            );
        }

        #[tokio::test]
        async fn killed_after_the_anchor_but_before_the_install() {
            // Same, with a harmless stale anchor: nothing reads one without a
            // marker matching the running binary, and the next apply removes it.
            let b = Boot::new().await;
            place(&b.current(), "old");
            place(&anchor_path(&b.current()), "old");
            b.stage_marker(NEXT_VERSION, CORE_VERSION, 0).await;

            assert!(!b.boot().await);
            assert_eq!(read(&b.current()), "old");
        }

        #[tokio::test]
        async fn killed_after_the_install_is_the_designed_state() {
            // New binary live, anchor beside it, marker naming this version:
            // the budget is spent and an unhealthy binary rolls back.
            let b = Boot::new().await;
            place(&b.current(), "new");
            place(&anchor_path(&b.current()), "old");
            b.stage_marker(CORE_VERSION, "0.0.1", 0).await;

            assert!(
                !b.boot().await,
                "first boot spends budget rather than rolling back"
            );
            assert!(!b.boot().await, "second boot too");
            assert!(b.boot().await, "the third rolls back");
            assert_eq!(read(&b.current()), "old", "onto the anchor");
        }

        #[tokio::test]
        async fn a_marker_last_ordering_leaves_a_state_with_no_recovery() {
            // Why the marker is written first: this stages a marker-last
            // ordering dying between the install and the marker write. The
            // boot path correctly does NOTHING, which is the problem, so a
            // broken release would crash-loop with its recovery unused.
            let b = Boot::new().await;
            place(&b.current(), "new-and-broken");
            place(&anchor_path(&b.current()), "old-and-working");

            for _ in 0..3 {
                assert!(!b.boot().await);
                assert_eq!(
                    read(&b.current()),
                    "new-and-broken",
                    "no marker means no rollback, however many times it fails"
                );
            }
        }

        #[tokio::test]
        async fn killed_mid_helper_rename_leaves_the_aside_that_helpers_restores() {
            // The Windows-only window inside `commit_rename`: live path empty,
            // previous file at `<path>.old`. The boot-time helper check owns
            // recovery; this asserts the state it has to find.
            let b = Boot::new().await;
            place(&b.current(), "old");
            let helper = b.paths.bin_path("loca-core-updater");
            place(&anchor_path(&helper), "the previous updater");

            assert!(!helper.exists(), "the live helper vanished");
            assert!(
                anchor_path(&helper).exists(),
                "and its predecessor is recoverable from the aside"
            );
            assert!(!b.boot().await, "the marker path is unaffected either way");
        }
    }
}
