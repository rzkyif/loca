//! Boot-time rollback: decide whether the binary that is running deserves to
//! stay, and put the previous one back when it does not.
//!
//! This runs before the database opens and before anything binds, so it is the
//! only code in the package that can leave a machine with no working core. Two
//! consequences shape everything below.
//!
//! # 1. There is no user-visible diagnosis here, so the log is the evidence
//!
//! Nothing is serving, so there is no error body, no frame, no status. Whatever
//! this module writes is all anyone will ever have, which is why every branch
//! logs and why the branches that leave the machine unhealthy also write the
//! boot breadcrumb (`<root>/last-error.txt`). That file is not a new idea: the
//! Client already reads it (`loca-client/src/tauri/src/commands/pairing.rs:72-89`)
//! precisely to "explain an otherwise-opaque connection failure", and the core
//! clears it on the next successful bind (`main.ts:316`), so it cannot go
//! stale. The oracle logs only, and a user whose rollback could not run sees an
//! opaque connection timeout instead of the reason. That divergence is
//! deliberate and is the one addition this module makes.
//!
//! # 2. A rollback that leaves no working binary is worse than no rollback
//!
//! The `<bin>.old` anchor is the entire recovery path, and it is reachable in a
//! real interrupted update for it to be missing OR present and unusable. The
//! oracle checks only that it EXISTS (`rollback.ts:158`), which is not the same
//! question, and the gap is reachable rather than theoretical:
//! `swapBinaryInPlace` falls back to `Deno.copyFile(current, old).catch(() => {})`
//! on a filesystem without hard links (`self-updater.ts:355-357`), and that
//! `catch` swallows a copy that ran out of disk. The truncated or empty file it
//! leaves passes `Deno.stat`. Rolling onto it replaces a binary that MIGHT work
//! with one that provably cannot exec, and the marker is deleted either way, so
//! there is no second attempt.
//!
//! So [`inspect_anchor`] refuses an anchor that is empty, is not a regular
//! file, or (on unix) is not executable, and treats it exactly like a missing
//! one: log, breadcrumb, clear the marker, and carry on booting the new binary.
//! Carrying on is strictly better than the alternative, because a binary that
//! failed to reach the healthy checkpoint may still start, and a zero-byte one
//! cannot.
//!
//! **The limit of that check, stated rather than implied.** A truncated but
//! non-empty anchor is not detectable here: nothing at rollback time knows what
//! the old binary's size should have been. The defence for that case belongs to
//! the anchor's CREATION, which must verify the copy against its source rather
//! than swallowing the error, and that is `swap.rs`'s obligation.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::paths::Paths;
use crate::update::marker::{self, MarkerState, UpdateMarker};

/// How many boots the new binary gets to reach its healthy checkpoint.
///
/// Greater than one on purpose (`rollback.ts:36-38`): a single boot INTERRUPTED
/// before the checkpoint (an external restart, a reboot during startup) is not
/// a crash, and mistaking it for one silently downgrades a working install.
pub const MAX_BOOT_ATTEMPTS: u32 = 2;

/// Decide what to do about a pending update, before anything else boots.
///
/// Returns `true` when a rollback happened, which the caller answers by exiting
/// **non-zero** so the supervisor relaunches the restored binary. A clean exit
/// would satisfy launchd's `KeepAlive` but not systemd's `Restart=on-failure`
/// or Task Scheduler's, and the core would simply stay down.
pub async fn handle_update_marker_on_boot(config: &Config, paths: &Paths) -> bool {
    let marker = match marker::read(paths).await {
        MarkerState::Absent => return false,
        MarkerState::Unreadable(reason) => {
            // Continue booting, which is the safe answer, but say so. Left on
            // disk rather than deleted: `commit_update` clears it on the first
            // healthy boot, which is exactly when it stops mattering, and
            // deleting it here would destroy the evidence before anyone read
            // this line.
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
        // Not fatal, and the direction matters: an unbumped marker means the
        // budget is not spent, so a genuinely broken binary is retried instead
        // of rolled back. That is the safe failure, and it is the one worth
        // saying out loud because it is otherwise invisible.
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
/// Called once the HTTP listener is bound, which implies the database opened
/// and the TLS key unsealed. Committing at that checkpoint rather than after a
/// fixed uptime is what makes an ordinary restart of a working binary unable to
/// trip a rollback: by the time it restarts, the marker is already gone
/// (`rollback.ts:11-13`). Idempotent, and a no-op with no update pending.
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
        // Absent is normal on a first install, where a marker can exist with no
        // anchor behind it.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(
            target: "loca::update.rollback",
            "update committed, but the rollback anchor {} could not be removed: {err}",
            old_bin.display(),
        ),
    }
    marker::delete(paths).await;
}

/// Whether the anchor can be rolled onto.
///
/// Visible to `swap.rs` on purpose: the module that CREATES an anchor and the
/// module that reads one must agree about what "usable" means, and two copies
/// of that predicate is how they come to disagree (D-049).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Anchor {
    Missing,
    /// Present and provably unusable, with the reason for the log.
    Unusable(String),
    Usable {
        size: u64,
    },
}

/// Inspect the anchor without trusting its mere existence. See the module docs
/// for why, and for the one case this cannot catch.
pub(crate) fn inspect_anchor(path: &Path) -> Anchor {
    let Ok(meta) = std::fs::metadata(path) else {
        return Anchor::Missing;
    };
    if !meta.is_file() {
        return Anchor::Unusable("it is not a regular file".into());
    }
    if meta.len() == 0 {
        // What an interrupted copy leaves, and what the oracle's existence
        // check waves through.
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

/// Put the previous binary back, or explain why that was not possible.
///
/// Returns `true` only when the previous binary is now at the live path.
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
            // Refusing is the whole point: swapping this in would replace a
            // binary that might start with one that provably cannot.
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

    // Leftovers from an earlier attempt. Both are best-effort: a stale one is
    // about to be overwritten anyway, and failing to remove it must not stop a
    // recovery.
    let _ = tokio::fs::remove_file(&broken_bin).await;
    let _ = tokio::fs::remove_file(&staged_old).await;
    warn_if_disk_bound(paths, size).await;

    // Copy-first staging, so the original anchor survives until the install
    // rename has succeeded:
    //   1. copy   oldBin     -> stagedOld
    //   2. rename currentBin -> brokenBin   (aside; atomic on one filesystem)
    //   3. rename stagedOld  -> currentBin  (install)
    //   4. remove oldBin                    (committed; the original is fungible)
    let used_copy = match tokio::fs::copy(&old_bin, &staged_old).await {
        Ok(_) => true,
        Err(err) => {
            // The older two-rename pattern, which loses `currentBin` if the
            // install rename then fails. Kept as the degraded path rather than
            // aborting, because a disk too full to copy is exactly when a
            // rollback is most needed.
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
        // The broken binary is still at the live path, so the supervisor has
        // something to launch. Nothing was moved, so this is the least-bad
        // failure and the marker is cleared so it is not retried forever.
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
        // Committed: the original anchor is fungible now that its copy is live.
        let _ = tokio::fs::remove_file(&old_bin).await;
    }
    marker::delete(paths).await;
    // `broken_bin` is deliberately left on disk. It is the only artefact of a
    // release that could not start, and it is what someone diagnosing this
    // afterwards has to look at.
    tracing::info!(
        target: "loca::update.rollback",
        "rolled back to v{}; exiting so the supervisor relaunches it. The binary that \
         failed is kept at {} for diagnosis.",
        marker.previous_version,
        broken_bin.display(),
    );
    true
}

/// Log the reason, leave it where the Client can read it, clear the marker, and
/// report that no rollback happened.
///
/// Always returns `false`, so a caller cannot accidentally treat an
/// unrecoverable state as a successful rollback and exit as though something
/// had been restored.
async fn unrecoverable(paths: &Paths, message: String) -> bool {
    tracing::error!(target: "loca::update.rollback", "{message}");
    let scrubbed = crate::log::scrub_secrets(&message);
    if let Err(err) = crate::files::write_atomic(
        paths.boot_error_file(),
        format!("{scrubbed}\n").into_bytes(),
    )
    .await
    {
        // Best-effort by construction: failing to write the breadcrumb must not
        // replace the real reason with a worse one.
        tracing::warn!(
            target: "loca::update.rollback",
            "could not write the boot-failure breadcrumb: {err}",
        );
    }
    // Cleared so the next boot does not repeat a rollback that cannot work.
    // Without this the core retries the same impossible recovery on every
    // launch and never says anything new.
    marker::delete(paths).await;
    false
}

/// Warn before the swap when the volume is already known to be short.
///
/// Informational only, and the caller tolerates not knowing: the swap has its
/// own failure fallback. It exists so the reason appears BEFORE the generic
/// "copy-first staging failed" line rather than having to be inferred from it.
/// `statvfs` rather than the oracle's `df -Pk` subprocess, which is the same
/// answer without parsing another program's output.
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
    // The oracle skips the precheck on Windows too. The swap's own failure path
    // is the fallback either way.
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

/// Step 3 of the swap, behind a test-only fault injector.
///
/// The install rename is the step whose failure has the most elaborate recovery
/// (put the aside binary back, so the supervisor still finds something) and it
/// is the one failure that cannot be staged portably from outside: by the time
/// it runs, the destination has just been vacated by step 2, both paths are on
/// one filesystem, and nothing a test can arrange beforehand makes it fail.
///
/// So it gets an injector rather than no coverage, following D7's
/// `panic_next_worker()`. It compiles out of every production build, and what
/// it exercises is the REAL recovery branch rather than a stand-in for it.
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

/// `<bin>.old`: the platform's real binary name plus the suffix.
///
/// Channel-suffixed, so a latest install rolls back `loca-core-latest` rather
/// than stable's `loca-core` (`rollback.ts:82-87`).
fn old_binary_path(config: &Config, paths: &Paths) -> PathBuf {
    with_suffix(&paths.bin_path(&core_binary_name(config)), ".old")
}

/// The on-disk name of this core's own binary.
///
/// `Channel::core_binary_name` rather than a local re-derivation of the suffix
/// rule: the channel suffix and the `.exe` suffix are one composition and two
/// copies of it is how a latest install comes to roll back stable's binary.
fn core_binary_name(config: &Config) -> String {
    config.channel.core_binary_name("loca-core")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::{Channel, CORE_VERSION};
    use crate::update::testing;

    /// A core root with a bin dir, plus whatever binaries the case stages.
    ///
    /// Holds the shared rollback guard for its whole life, so every test that
    /// drives a boot decision serialises whether or not it knows about the
    /// install-rename injector. See `testing::rollback_guard`.
    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        config: Config,
        _guard: crate::update::testing::RollbackGuard,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // Disarmed here rather than in a separate guard, so a test cannot
            // leave the injector set for whoever takes the lock next.
            testing::set_install_rename_failure(false);
        }
    }

    impl Fixture {
        async fn new() -> Self {
            Self::on_channel(Channel::Stable).await
        }

        /// Make step 3 of the swap fail. Only reachable from a fixture, which
        /// is what guarantees the lock is held while it is armed.
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

        /// Write an executable file with recognisable contents, so a test can
        /// tell WHICH binary ended up at the live path. Asserting only that a
        /// file exists there would pass whichever one it is, which is the
        /// entire question.
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
    }

    // --- the decision ------------------------------------------------------

    #[tokio::test]
    async fn no_marker_is_a_silent_no_op() {
        let f = Fixture::new().await;
        assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
    }

    // The retry budget is pinned by three cases staged with LITERAL attempt
    // counts, never with `MAX_BOOT_ATTEMPTS`. Deriving a fixture from the
    // constant under test makes the fixture move with it, so halving the
    // constant scales the test and the mutation survives (D-050 case 5, and
    // D12's phase-1 lesson).
    //
    // The three together admit exactly one threshold. Only the middle one can
    // see a change from 2 to 1, which is why it exists: staging attempt 0 and
    // asserting no rollback passes under both values, and that is precisely the
    // hole a mutation found in the first version of this suite.

    #[tokio::test]
    async fn one_interrupted_boot_is_retried_rather_than_rolled_back() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "old");
        f.stage_marker(CORE_VERSION, "0.0.1", 0).await;

        assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
        assert_eq!(f.live_contents(), "new", "nothing was swapped");

        let MarkerState::Present(marker) = marker::read(&f.paths).await else {
            panic!("d13: the marker was cleared on the first boot");
        };
        assert_eq!(marker.attempts, 1, "the attempt was recorded");
    }

    #[tokio::test]
    async fn a_second_interrupted_boot_is_still_retried() {
        // The discriminating case. An external restart during startup can
        // happen twice (a reboot mid-upgrade, a supervisor bouncing the unit),
        // and the budget exists so that is not read as a broken binary.
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "old");
        f.stage_marker(CORE_VERSION, "0.0.1", 1).await;

        assert!(
            !handle_update_marker_on_boot(&f.config, &f.paths).await,
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
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "old");
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(handle_update_marker_on_boot(&f.config, &f.paths).await);
        assert_eq!(f.live_contents(), "old", "the previous binary is live");
        assert!(matches!(marker::read(&f.paths).await, MarkerState::Absent));
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

        assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
        assert_eq!(f.live_contents(), "new");
        assert!(matches!(marker::read(&f.paths).await, MarkerState::Absent));
    }

    #[tokio::test]
    async fn a_marker_for_the_version_we_already_rolled_back_to_is_cleared() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.stage_marker("9.9.9", CORE_VERSION, 2).await;

        assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
        assert!(matches!(marker::read(&f.paths).await, MarkerState::Absent));
    }

    #[tokio::test]
    async fn an_unreadable_marker_continues_the_boot_and_is_left_for_the_commit() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        tokio::fs::write(f.paths.update_marker_file(), "{ not json")
            .await
            .unwrap();

        assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
        assert!(
            matches!(marker::read(&f.paths).await, MarkerState::Unreadable(_)),
            "left on disk, because commit_update clears it on the first healthy boot"
        );
    }

    // --- the anchor, which is the whole recovery path ----------------------

    #[tokio::test]
    async fn a_missing_anchor_leaves_the_current_binary_alone_and_says_why() {
        // The oracle's behaviour, plus the breadcrumb. Rolling back is
        // impossible, so the only choice is whether anyone finds out.
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
        assert_eq!(f.live_contents(), "new", "nothing was destroyed");
        assert!(
            matches!(marker::read(&f.paths).await, MarkerState::Absent),
            "cleared, so the impossible recovery is not retried every launch"
        );
        let crumb = f.breadcrumb().await.expect("a breadcrumb was written");
        assert!(crumb.contains("Reinstall"), "{crumb}");
    }

    #[tokio::test]
    async fn an_empty_anchor_is_refused_rather_than_installed() {
        // The reachable case the oracle's existence check waves through: the
        // copy fallback in `swapBinaryInPlace` swallows its error, so a copy
        // that ran out of disk leaves a zero-byte file that `stat` accepts.
        // Installing it would replace a binary that might start with one that
        // provably cannot exec.
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "");
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
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

        assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
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

        assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
        assert_eq!(f.live_contents(), "new");
        let crumb = f.breadcrumb().await.expect("a breadcrumb was written");
        assert!(crumb.contains("executable"), "{crumb}");
    }

    #[test]
    fn a_usable_anchor_is_the_only_state_that_permits_a_swap() {
        // Reading the states directly, so the three refusals above cannot all
        // be passing for one shared reason.
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
        // Asserts the END STATE, and deliberately does NOT claim to cover the
        // `set_executable` call that follows the install rename. It cannot:
        // `inspect_anchor` already refuses a non-executable anchor, and
        // `fs::copy` and `fs::rename` both preserve the mode on every
        // filesystem a test can create here, so the restored binary is
        // executable with or without that call. Deleting it leaves this green,
        // and that was watched rather than assumed.
        //
        // The call is kept anyway, for the one case neither this test nor that
        // check covers: a filesystem that does not preserve permissions across
        // a copy. Recorded as a deliberate survivor rather than deleted on the
        // evidence that nothing fails, which is the mistake D-050's `biased;`
        // note exists to prevent.
        use std::os::unix::fs::PermissionsExt as _;
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "old");
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(handle_update_marker_on_boot(&f.config, &f.paths).await);
        let mode = std::fs::metadata(f.current()).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "mode was {mode:o}");
    }

    // --- the failure branches inside the swap ------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_aside_leaves_the_current_binary_where_the_supervisor_expects_it() {
        // Step 2 fails. Nothing has moved yet, so the least-bad outcome is the
        // broken binary staying live, and the marker cleared so this is not
        // retried on every launch forever.
        use std::os::unix::fs::PermissionsExt as _;
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "old");
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;
        // A read-only bin dir makes the rename fail without making the anchor
        // unreadable, which is the only way to reach this branch honestly.
        let bin_dir = f.paths.bin_dir();
        std::fs::set_permissions(&bin_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

        let rolled_back = handle_update_marker_on_boot(&f.config, &f.paths).await;

        std::fs::set_permissions(&bin_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!rolled_back);
        assert_eq!(f.live_contents(), "new", "something is still runnable");
        assert!(matches!(marker::read(&f.paths).await, MarkerState::Absent));
        assert!(f.breadcrumb().await.is_some());
    }

    #[tokio::test]
    async fn a_failed_install_puts_the_aside_binary_back() {
        // Step 3 fails after step 2 has already moved the live binary away, so
        // for a moment there is NO binary at the install path. The revert is
        // the only thing between that and a machine the supervisor cannot
        // launch at all, and it is the branch a reader is most likely to
        // simplify away, because on the happy path it never runs.
        let f = Fixture::new().await;
        f.fail_install_rename();
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "old");
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
        assert_eq!(
            f.live_contents(),
            "new",
            "the broken binary is put back, because something runnable beats nothing"
        );
        assert!(
            !with_suffix(&f.anchor(), ".staged").exists(),
            "the staging copy is cleaned up rather than left to confuse the next attempt"
        );
        assert!(matches!(marker::read(&f.paths).await, MarkerState::Absent));
        let crumb = f.breadcrumb().await.expect("a breadcrumb was written");
        assert!(crumb.contains("put back"), "{crumb}");
    }

    #[tokio::test]
    async fn a_failed_copy_falls_back_to_the_two_rename_swap() {
        // A disk too full to copy is exactly when a rollback matters most, so
        // the degraded path has to work rather than abort. Staged by making the
        // staging path a non-empty directory, which is a real thing an
        // interrupted attempt can leave and which `copy` cannot write over.
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "old");
        let staged = with_suffix(&f.anchor(), ".staged");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("occupied"), "x").unwrap();
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(handle_update_marker_on_boot(&f.config, &f.paths).await);
        assert_eq!(f.live_contents(), "old", "the rollback still happened");
    }

    #[tokio::test]
    async fn a_rollback_on_a_non_stable_channel_uses_that_channels_binary() {
        // The anchor is channel-suffixed, so a latest install must roll back
        // `loca-core-latest` and not stable's `loca-core`. Getting this wrong
        // is invisible on the default channel, which is where it would be
        // tested by default.
        let f = Fixture::on_channel(Channel::Latest).await;
        assert!(f.current().to_string_lossy().contains("loca-core-latest"));
        f.place(&f.current(), "new-latest");
        f.place(&f.anchor(), "old-latest");
        f.stage_marker(CORE_VERSION, "0.0.1", 2).await;

        assert!(handle_update_marker_on_boot(&f.config, &f.paths).await);
        assert_eq!(f.live_contents(), "old-latest");
    }

    // --- commit ------------------------------------------------------------

    #[tokio::test]
    async fn commit_removes_both_the_marker_and_the_anchor() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "old");
        f.stage_marker(CORE_VERSION, "0.0.1", 1).await;

        commit_update(&f.config, &f.paths).await;

        assert!(matches!(marker::read(&f.paths).await, MarkerState::Absent));
        assert!(!f.anchor().exists(), "the anchor is gone");
        assert_eq!(f.live_contents(), "new");
    }

    #[tokio::test]
    async fn commit_is_a_no_op_with_no_update_pending() {
        // An ordinary restart of a working core reaches the checkpoint on every
        // boot. Removing the anchor unconditionally would delete the recovery
        // path of an update that is still on trial.
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
        // The self-healing path the read side relies on: a marker nobody can
        // parse is cleared by the first boot that proves it does not matter.
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        tokio::fs::write(f.paths.update_marker_file(), "{ not json")
            .await
            .unwrap();

        commit_update(&f.config, &f.paths).await;
        assert!(matches!(marker::read(&f.paths).await, MarkerState::Absent));
    }

    #[tokio::test]
    async fn commit_tolerates_a_first_install_with_no_anchor() {
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.stage_marker(CORE_VERSION, "0.0.1", 1).await;

        commit_update(&f.config, &f.paths).await;
        assert!(matches!(marker::read(&f.paths).await, MarkerState::Absent));
    }

    #[tokio::test]
    async fn a_committed_update_cannot_be_rolled_back_by_a_later_restart() {
        // The property the healthy-checkpoint commit exists for
        // (`rollback.ts:11-13`), asserted end to end rather than inferred: boot,
        // commit, then boot again twice more and confirm nothing rolls back.
        // Committing after a fixed uptime instead would leave this reachable.
        let f = Fixture::new().await;
        f.place(&f.current(), "new");
        f.place(&f.anchor(), "old");
        f.stage_marker(CORE_VERSION, "0.0.1", 0).await;

        assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
        commit_update(&f.config, &f.paths).await;

        for _ in 0..2 {
            assert!(!handle_update_marker_on_boot(&f.config, &f.paths).await);
            assert_eq!(f.live_contents(), "new");
        }
    }
}
