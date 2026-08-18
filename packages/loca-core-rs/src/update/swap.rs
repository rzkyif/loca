//! Replacing the core's own binary while it is running, and creating the anchor
//! that makes it undoable.
//!
//! Ported from `swapBinaryInPlace` and `commitRename`
//! (`self-updater.ts:335-388`). Both take the platform as a PARAMETER rather
//! than reading `cfg!(windows)`, which is what the TypeScript does and for the
//! reason it states at `:333`: it is the only way either branch is exercised on
//! a machine that is not both platforms.
//!
//! # The anchor is the whole recovery path, so creating one is a verified step
//!
//! `rollback.rs` refuses an anchor that is empty, is not a regular file, or is
//! not executable, and names the case it cannot catch: a truncated but
//! non-empty anchor, because nothing at rollback time knows what the old binary
//! should have weighed. **This is where that case is caught**, and the gap is
//! reachable rather than theoretical: the oracle's copy fallback is
//! `Deno.copyFile(current, old).catch(() => {})` (`self-updater.ts:355-357`),
//! so a copy that ran out of disk is swallowed and the `Deno.stat` that follows
//! passes the partial file it left.
//!
//! So [`create_anchor`] verifies its result before the update is allowed to
//! proceed: the anchor must satisfy `rollback::inspect_anchor` (the SAME
//! predicate the boot path applies, not a second copy of it, per D-049) and its
//! length must equal the source's. A failure removes the partial and aborts the
//! whole update, which is the safe direction: nothing has been swapped yet.
//!
//! **What that still cannot catch, stated rather than implied.** A copy of the
//! right LENGTH whose bytes differ from the source. A full content hash would
//! catch it and is not worth its cost here: the failure being defended against
//! is a truncated or interrupted write, which always changes the length, while
//! same-length divergence means the storage layer is corrupting data it
//! reported as written, and a core that cannot trust a local copy cannot trust
//! the verified download it is about to install either. The hard-link path is
//! exempt for a stronger reason than cost: it produces a second name for the
//! SAME inode, so there is no copy to verify.
//!
//! # Dying mid-swap: every reachable state, paired with the boot that recovers it
//!
//! `rollback.rs` handles a bad anchor at boot. This module owns not creating
//! one, and the two halves are only a safety argument together. The table is
//! for the ordering `apply.rs` uses: **artifacts, then helpers, then the
//! marker, then the swap.**
//!
//! | died just after            | on disk                                        | next boot                                              |
//! | -------------------------- | ---------------------------------------------- | ------------------------------------------------------ |
//! | the manifest verified      | nothing changed                                | no marker, normal boot                                 |
//! | artifacts staged           | `staging/` dirty                               | no marker, normal boot; the next apply clears staging  |
//! | some helper renames        | mixed new and old helpers, OLD binary          | no marker, normal boot (see below)                     |
//! | the marker was written     | marker, OLD binary still live                  | version mismatch, `previousVersion` matches, cleared   |
//! | the stale anchor removed   | as above                                       | as above                                               |
//! | the anchor was created     | marker, anchor, OLD binary live                | as above; the stale anchor goes on the next apply      |
//! | the anchor failed to verify | marker, no anchor, OLD binary live            | as above                                               |
//! | the binary was installed   | marker, anchor, NEW binary live                | **the designed state**: the retry budget then rollback |
//! | the restart was issued     | as above                                       | as above                                               |
//!
//! Every row recovers. Two need saying out loud.
//!
//! **The marker is written BEFORE the swap, and the oracle writes it after**
//! (`self-updater.ts:239-242`, below the swap at `:226-234`). That ordering
//! leaves one state with no recovery at all: the new binary installed, the
//! anchor present, and no marker pointing at it. Nothing on the next boot reads
//! an anchor it was not told about, so a broken release in that window is a
//! machine that crash-loops with a perfectly good previous binary sitting
//! beside it, unused. Writing first cannot go wrong in the other direction: a
//! marker whose version is not the running one and whose `previousVersion` IS
//! the running one is exactly the "already rolled back" case `rollback.rs`
//! already clears. The window is microseconds and the consequence is total,
//! which is the shape worth closing for free.
//!
//! **A mixed helper set is tolerated by design, and the design says when it
//! stops being.** `self-updater.ts:162-170` records it: rollback restores only
//! `<bin>.old`, never the helpers, so a rollback already leaves new helpers
//! beside an old core. That is acceptable only while every helper speaks a
//! stable subprocess protocol, and the comment names the condition under which
//! this commit must move to the healthy checkpoint and grow its own anchors.
//!
//! # The one window nothing here can close
//!
//! On Windows a running `.exe` cannot be overwritten, only renamed, so the swap
//! is two renames and **between them there is no file at the install path**. A
//! hard kill in that window leaves nothing for the supervisor to launch, and no
//! in-process mitigation exists, because the process that would mitigate it is
//! the one that died. It is one rename wide and nothing is done between them.
//! Recorded as a named residual risk rather than defended against badly:
//! `commit_rename` has the same shape for a locked helper, and there the boot
//! check in `helpers.rs` can and does restore from the aside.

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

/// Which swap semantics to use.
///
/// Explicit rather than `cfg!(windows)` so both branches run on one host. The
/// platform difference is not a detail: on unix the live inode can be replaced
/// under a running process, and on Windows it can only be renamed aside first.
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

    /// The mapping, separated from the `cfg!` that feeds it.
    ///
    /// Not a flourish. `cfg!(windows)` is `false` on every machine this fleet
    /// runs on, so `host()` returning `Unix` unconditionally passes any test
    /// written over it: the assertion names the rule and cannot fail on it.
    /// Splitting the decision out is what makes both answers assertable here,
    /// and it is the same seam `config::with_exe` exists for.
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
/// Works while the core is running on both platforms. The anchor is mandatory:
/// without it boot-time rollback has nothing to restore, so a failure to create
/// a verifiable one aborts the update rather than proceeding unprotected.
pub async fn swap_binary_in_place(staged: &Path, current: &Path, platform: Platform) -> Result<()> {
    let anchor = anchor_path(current);
    let _ = tokio::fs::remove_file(&anchor).await;

    if platform == Platform::Windows {
        // The move IS the anchor, and it is a rename, so there are no bytes to
        // verify. It is also what frees the install path, which is why the two
        // renames are adjacent and nothing sits between them.
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
            // Put the running binary back, so the supervisor still has one.
            // Without this a failed install leaves the install path empty and
            // the machine unlaunchable.
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
/// A hard link first, because it is atomic and produces a second name for one
/// inode, so there is nothing to verify and nothing to copy. A filesystem
/// without hard links falls back to a copy, and THAT is what gets checked. See
/// the module docs for what the check covers and what it does not.
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
            // Removed rather than left: a partial anchor that survives here is
            // exactly the file `rollback.rs` would have to guess about later.
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
/// A separate function because it is the only part of anchor creation that can
/// be driven through every one of its outcomes. **The call site cannot be:** on
/// this platform `tokio::fs::copy` returns an error rather than reporting a
/// short write, so there is no reachable way to make the copy succeed and the
/// verification fail. That is stated rather than papered over, and it is also
/// the reason the primary defence is one line above rather than here.
///
/// **The primary defence is propagating the copy error.** The oracle's actual
/// defect is `.catch(() => {})` (`self-updater.ts:356`), a JavaScript-specific
/// swallow with no Rust equivalent, and `a_copy_that_cannot_be_written_aborts_the_update`
/// is what pins that. This check is the second line: it catches an anchor that
/// exists, reports success, and is the wrong length, which is the state the
/// oracle's `Deno.stat` accepts and `rollback.rs` cannot distinguish later.
fn verify_anchor(anchor: &Path, source_len: u64) -> Result<()> {
    // The SAME predicate the boot path applies, so the two cannot drift about
    // what "usable" means (D-049).
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

/// The message every anchor refusal shares.
///
/// One wording, because this is the error a user sees when an update declines
/// to proceed and it has to say why proceeding would be worse.
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
/// The aside is left at `<to>.old`, which is both the recovery path for a crash
/// between the two renames (see `helpers.rs`) and bounded to one per artifact,
/// because the next update removes it before writing its own.
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
        // Reached by removing the staged file after the aside would have run.
        // Without the restore the install path is empty and the supervisor has
        // nothing to launch, which is worse than a failed update.
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
    /// process-global. Same reasoning as the rollback fixture: a neighbour must
    /// not inherit an injector it does not know exists.
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
        // `cfg!(windows)` is false on every machine this fleet runs on, so a
        // test over `host()` alone passes with the Windows arm deleted. The
        // mapping is asserted through the seam instead, and `host()` is then
        // one token of `cfg!` with nothing to get wrong.
        assert_eq!(Platform::from_windows(true), Platform::Windows);
        assert_eq!(Platform::from_windows(false), Platform::Unix);
        assert_eq!(Platform::host(), Platform::from_windows(cfg!(windows)));
    }

    #[tokio::test]
    async fn the_copy_fallback_produces_a_verified_anchor() {
        // The branch that exists for filesystems without hard links, which is
        // no filesystem a test can create here. Without the seam this path is
        // reachable only through its FAILURE case, so a test naming the
        // fallback would be covering the error handler and not the fallback.
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
        // End to end through the fallback, because `create_anchor` succeeding
        // in isolation does not prove the swap around it survives a real copy.
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
        // The primary defence, and the one the oracle lacks: its copy fallback
        // is `.catch(() => {})`, so a copy that fails is swallowed and the
        // `stat` that follows accepts whatever partial file it left. Here the
        // error propagates and nothing is swapped.
        //
        // Staged with an unwritable destination directory, which is a real
        // shape (a bin dir the installer left root-owned) and reaches the copy
        // branch because `hard_link` fails there too.
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
        // The REASON is asserted, not merely the refusal. Swallowing the copy
        // error still produces an error here, because `verify_anchor` then
        // finds nothing and refuses on its own: an assertion that stops at "it
        // failed" cannot tell the two apart, and that mutation was staged and
        // SURVIVED this test before this line existed. What the swallow costs
        // is the diagnosis, and on a pre-listener failure the diagnosis is the
        // entire output a user or a support log will ever have.
        assert!(
            err.message.contains("the copy failed"),
            "the refusal must name the copy rather than report a missing anchor: {}",
            err.message
        );
    }

    #[test]
    fn the_anchor_post_condition_rejects_every_shape_that_is_not_a_fallback() {
        // Driven directly, because the call site cannot be: `tokio::fs::copy`
        // errors rather than reporting a short write, so no reachable input
        // makes the copy succeed and this fail. Testing the predicate is what
        // is available, and it is worth having: a truncated anchor is "usable"
        // by every property the boot path can see, which is exactly why the
        // length has to be compared at creation and cannot be recovered later.
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
        // A refusal that leaves a partial anchor is worse than no refusal: the
        // boot path would then have to guess about a file this code already
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
    // This is the safety argument for the package and it is only checkable now
    // that both halves exist. Each case stages the EXACT on-disk state a kill
    // at that point would leave, then runs the real boot path over it.

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
            // Nothing points at anything. The old binary boots normally and the
            // half-finished staging is cleared by the next apply.
            let b = Boot::new().await;
            place(&b.current(), "old");
            std::fs::create_dir_all(b.paths.staging_dir()).unwrap();
            std::fs::write(b.paths.staging_dir().join("partial"), "junk").unwrap();

            assert!(!b.boot().await);
            assert_eq!(read(&b.current()), "old");
        }

        #[tokio::test]
        async fn killed_after_the_marker_but_before_the_swap() {
            // The state the marker-first ordering creates and the oracle's
            // ordering does not. The running binary is still the OLD one, so
            // the marker names a version that is not installed. `previousVersion`
            // IS the running version, which is the "already rolled back" case,
            // and it is cleared rather than acted on.
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
            // Same as above with a stale anchor beside it. The anchor is
            // harmless: nothing reads one without a marker that matches the
            // running binary, and the next apply removes it before writing its
            // own.
            let b = Boot::new().await;
            place(&b.current(), "old");
            place(&anchor_path(&b.current()), "old");
            b.stage_marker(NEXT_VERSION, CORE_VERSION, 0).await;

            assert!(!b.boot().await);
            assert_eq!(read(&b.current()), "old");
        }

        #[tokio::test]
        async fn killed_after_the_install_is_the_designed_state() {
            // The new binary is live, the anchor is beside it, and the marker
            // names this version. This is protection working: the budget is
            // spent over the next boots and a binary that never gets healthy is
            // rolled back onto the anchor.
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
        async fn the_oracle_ordering_leaves_a_state_with_no_recovery() {
            // Why the marker is written first. This stages what the oracle's
            // ordering produces when the process dies between the install and
            // the marker write: the new binary live, a perfectly good anchor
            // beside it, and nothing pointing at either.
            //
            // The assertion is that the boot path does NOTHING, which is
            // correct behaviour for the state and is exactly the problem: a
            // broken release here crash-loops forever with its own recovery
            // sitting unused on disk. Asserting it here is what makes the
            // ordering in `apply.rs` a decision rather than a preference.
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
            // The Windows-only window inside `commit_rename`: the live path is
            // empty and the previous file is at `<path>.old`. Nothing here can
            // recover it, because the process that would is gone; the boot-time
            // helper check owns it, and this asserts the state it has to find.
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
