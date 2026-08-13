//! Per-platform resolution of the client's on-disk data root.
//!
//! - Desktop: `~/.loca/<channel>/client` (channel-isolated under the home
//!   directory, matching loca-core's layout).
//! - Android: the app's private data directory + `/client`. There is no `$HOME`
//!   on Android, so the path comes from Tauri's `app_data_dir()` (already
//!   sandboxed per app + channel applicationId).
//!
//! The file-based commands (client_files, client_storage, keychain) take the
//! `AppHandle` Tauri injects and resolve through here so the JS-facing command
//! signatures stay identical across platforms.

use crate::error::AppResult;
use std::path::PathBuf;
use tauri::AppHandle;

#[cfg(target_os = "android")]
use tauri::Manager;

/// The client's per-channel data root for the current platform.
pub fn client_root(handle: &AppHandle) -> AppResult<PathBuf> {
    #[cfg(target_os = "android")]
    {
        Ok(handle.path().app_data_dir()?.join("client"))
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = handle;
        let home = std::env::home_dir().ok_or_else(|| {
            crate::error::AppError::external("could not determine home directory")
        })?;
        Ok(crate::channel::channel_root(&home).join("client"))
    }
}
