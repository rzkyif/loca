//! The boot-time helper-binary check, narrowed to the one helper that stays.
//!
//! The Deno core refuses to start without four helpers (`binaries/helpers.ts`)
//! and says why: each has a quiet runtime fallback that is right when the
//! helper RUNS and its OS facility is unavailable, and wrong when the binary is
//! absent. A missing binary means a broken install, and the fallbacks hide it as
//! file-backed secrets, guessed hardware, and tool workers spawned without
//! permission prompts.
//!
//! The Rust core absorbs three of the four (keychain F5, hwinfo D6, ptyhost
//! D12), so this narrows to `loca-core-updater` and keeps the reasoning: refuse
//! rather than silently degrade. Without it, background-mode self-update has no
//! relauncher at all, which `apply` already treats as fatal at apply time.
//! Refusing at boot turns a failure discovered mid-update into one discovered at
//! start. D-025.
//!
//! # Called from the desktop frontend, not from `boot()`
//!
//! `boot()` is shared with the mobile embed (M1), and a phone has no updater
//! binary and no self-update, so a check inside `boot()` would make every
//! mobile core refuse to start. The oracle's own call site is `main.ts:135`,
//! which is the desktop entry point, so this is both the mobile-safe placement
//! and the one that matches.
//!
//! # It repairs the one state the swap can leave
//!
//! `swap::commit_rename` moves a locked Windows target aside to `<path>.old`
//! before renaming the new one in, and a hard kill between those two renames
//! leaves NO file at the live path with the previous one recoverable beside it.
//! Nothing in the swap can fix that, because the process that would is the one
//! that died. So the check does not merely report a missing updater: when the
//! aside is present it restores it and continues.
//!
//! **What that does not cover, stated rather than implied.** Only the helper
//! this check names. A worker script or a future helper left in the same state
//! is not restored here, because nothing at boot knows which of them should
//! exist; the general answer is for the update to record what it was committing
//! before it commits it, which is more machinery than one helper justifies
//! today. If a second helper ever returns, this is the code that has to grow
//! rather than be copied.

use std::path::Path;

use crate::config::Config;
use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;
use crate::update::swap::anchor_path;

/// The helpers a core of this generation cannot start without.
///
/// One entry, and it is a list rather than a constant so that adding the second
/// one is an edit to data instead of a rewrite of the logic.
const REQUIRED_HELPERS: &[&str] = &["loca-core-updater"];

/// Refuse to boot when a required helper is missing, after trying to recover
/// one that an interrupted update left aside.
pub async fn ensure_helper_binaries(config: &Config, paths: &Paths) -> Result<()> {
    let mut missing = Vec::new();
    for base in REQUIRED_HELPERS {
        let path = paths.bin_path(&config.channel.core_binary_name(base));
        if is_file(&path) {
            continue;
        }
        if restore_from_aside(&path).await {
            continue;
        }
        missing.push(*base);
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::InternalError,
        format!(
            "missing helper binaries in {}: {}. They ship in the release manifest and \
             are installed alongside core. Refusing to start rather than silently \
             degrade: without the updater, a self-update in background mode has no \
             relauncher and would leave the core down.",
            paths.bin_dir().display(),
            missing.join(", "),
        ),
    ))
}

/// Put `<path>.old` back at `path`, for the window described in the module docs.
///
/// Returns whether the live path is usable afterwards. Deliberately quiet about
/// a missing aside (that is the ordinary "helper was never installed" case) and
/// loud about one that exists and could not be moved, because that is a repair
/// that was available and did not happen.
async fn restore_from_aside(path: &Path) -> bool {
    let aside = anchor_path(path);
    if !is_file(&aside) {
        return false;
    }
    match tokio::fs::rename(&aside, path).await {
        Ok(()) => {
            tracing::warn!(
                target: "loca::update",
                "{} was missing and has been restored from {}: an update was \
                 interrupted between moving it aside and installing its replacement",
                path.display(),
                aside.display(),
            );
            true
        }
        Err(err) => {
            tracing::error!(
                target: "loca::update",
                "{} is missing and {} could not be moved back into place: {err}",
                path.display(),
                aside.display(),
            );
            false
        }
    }
}

fn is_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file())
        .unwrap_or(false)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::{Channel, CORE_VERSION};

    fn env(channel: Channel) -> (tempfile::TempDir, Config, Paths) {
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
        (dir, config, paths)
    }

    fn updater(config: &Config, paths: &Paths) -> std::path::PathBuf {
        paths.bin_path(&config.channel.core_binary_name("loca-core-updater"))
    }

    #[tokio::test]
    async fn a_present_updater_lets_the_core_start() {
        let (_d, config, paths) = env(Channel::Stable);
        std::fs::write(updater(&config, &paths), "updater").unwrap();
        ensure_helper_binaries(&config, &paths).await.unwrap();
    }

    #[tokio::test]
    async fn a_missing_updater_refuses_the_boot_and_says_why() {
        // Refusing rather than degrading is the whole decision, so the message
        // has to carry the reason: this is the last thing anyone sees before
        // the core does not come up.
        let (_d, config, paths) = env(Channel::Stable);
        let err = ensure_helper_binaries(&config, &paths).await.unwrap_err();
        assert!(err.message.contains("loca-core-updater"), "{}", err.message);
        assert!(err.message.contains("no relauncher"), "{}", err.message);
    }

    #[tokio::test]
    async fn the_check_is_channel_aware() {
        // A latest core needs `loca-core-updater-latest`, and stable's binary
        // sitting in the same dir does not satisfy it. Invisible on the default
        // channel, which is where this would otherwise be tested.
        let (_d, config, paths) = env(Channel::Latest);
        assert!(updater(&config, &paths)
            .to_string_lossy()
            .contains("loca-core-updater-latest"));
        std::fs::write(paths.bin_path("loca-core-updater"), "stable's").unwrap();

        let err = ensure_helper_binaries(&config, &paths).await.unwrap_err();
        assert!(err.message.contains("loca-core-updater"), "{}", err.message);
    }

    #[tokio::test]
    async fn an_updater_left_aside_by_an_interrupted_update_is_restored() {
        // The state `commit_rename` leaves if the process dies between its two
        // renames. Without this the core refuses to boot forever with its own
        // updater sitting one rename away.
        let (_d, config, paths) = env(Channel::Stable);
        let live = updater(&config, &paths);
        std::fs::write(anchor_path(&live), "the previous updater").unwrap();

        ensure_helper_binaries(&config, &paths).await.unwrap();

        assert_eq!(
            std::fs::read_to_string(&live).unwrap(),
            "the previous updater"
        );
        assert!(
            !anchor_path(&live).exists(),
            "the aside is consumed rather than left to be restored again next boot"
        );
    }

    #[tokio::test]
    async fn a_directory_at_the_aside_is_not_mistaken_for_a_recoverable_helper() {
        // `is_file` rather than an existence check, for the same reason
        // `inspect_anchor` refuses a directory: a path that exists is not a
        // binary, and treating it as one turns a clear refusal into a confusing
        // one at the next spawn.
        let (_d, config, paths) = env(Channel::Stable);
        let live = updater(&config, &paths);
        std::fs::create_dir_all(anchor_path(&live)).unwrap();

        assert!(ensure_helper_binaries(&config, &paths).await.is_err());
    }

    #[tokio::test]
    async fn a_present_helper_is_never_replaced_by_a_stale_aside() {
        // The ordering that matters: an aside left by an update that DID
        // complete must not overwrite the helper that completed it. Checking
        // the live path first is what makes the repair safe to run every boot.
        let (_d, config, paths) = env(Channel::Stable);
        let live = updater(&config, &paths);
        std::fs::write(&live, "the new updater").unwrap();
        std::fs::write(anchor_path(&live), "the old updater").unwrap();

        ensure_helper_binaries(&config, &paths).await.unwrap();
        assert_eq!(std::fs::read_to_string(&live).unwrap(), "the new updater");
    }
}
