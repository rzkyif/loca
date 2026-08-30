//! The boot-time helper-binary check: refuse to start without
//! `loca-core-updater` rather than silently degrade, since without it a
//! background-mode self-update has no relauncher.
//!
//! Called from the desktop frontend, not `boot()`, which the mobile embed
//! shares and which has no updater. It also repairs the one state the swap can
//! leave: a hard kill between `commit_rename`'s two renames leaves the helper
//! at `<path>.old` and nothing at the live path.

use std::path::Path;

use crate::config::Config;
use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;
use crate::update::swap::anchor_path;

/// The helpers a core of this generation cannot start without.
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

/// Put `<path>.old` back at `path`. Quiet about a missing aside (the helper was
/// never installed), loud about one that exists and could not be moved.
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
        let (_d, config, paths) = env(Channel::Stable);
        let err = ensure_helper_binaries(&config, &paths).await.unwrap_err();
        assert!(err.message.contains("loca-core-updater"), "{}", err.message);
        assert!(err.message.contains("no relauncher"), "{}", err.message);
    }

    #[tokio::test]
    async fn the_check_is_channel_aware() {
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
        let (_d, config, paths) = env(Channel::Stable);
        let live = updater(&config, &paths);
        std::fs::write(anchor_path(&live), "the previous updater").unwrap();

        ensure_helper_binaries(&config, &paths).await.unwrap();

        assert_eq!(
            std::fs::read_to_string(&live).unwrap(),
            "the previous updater"
        );
        assert!(!anchor_path(&live).exists(), "the aside is consumed");
    }

    #[tokio::test]
    async fn a_directory_at_the_aside_is_not_mistaken_for_a_recoverable_helper() {
        let (_d, config, paths) = env(Channel::Stable);
        let live = updater(&config, &paths);
        std::fs::create_dir_all(anchor_path(&live)).unwrap();

        assert!(ensure_helper_binaries(&config, &paths).await.is_err());
    }

    #[tokio::test]
    async fn a_present_helper_is_never_replaced_by_a_stale_aside() {
        let (_d, config, paths) = env(Channel::Stable);
        let live = updater(&config, &paths);
        std::fs::write(&live, "the new updater").unwrap();
        std::fs::write(anchor_path(&live), "the old updater").unwrap();

        ensure_helper_binaries(&config, &paths).await.unwrap();
        assert_eq!(std::fs::read_to_string(&live).unwrap(), "the new updater");
    }
}
