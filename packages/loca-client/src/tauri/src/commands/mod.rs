//! Tauri commands, organized by domain.
//!
//! Only platform-specific commands remain: everything server-side
//! (sessions, settings, sidecars, downloads, extensions, storage) moved to
//! loca-core and is reached over HTTP/WS by the frontend.
//!
//! Submodules:
//! - [`window`]: main-window visibility, positioning, global / input shortcuts.
//! - [`capture`]: monitor enumeration, full-monitor capture, region-capture overlay.
//! - [`fonts`]: installed system font enumeration.
//! - [`pairing`]: read on-disk admin token, install local core (CDN-dependent).
//! - [`client_files`]: per-concern JSON stores under ~/.loca/<channel>/client/
//!   (settings.json, cores.json, snippets/).
//! - [`keychain`]: per-core bearer token storage via the OS keychain.

// Shared commands (compiled on every platform).
pub mod client_files;
pub mod client_storage;
pub mod keychain;
pub mod net;
pub mod paths;
pub mod process;

// Desktop-only commands. Each backs a host feature with no Android equivalent
// (windowing / global shortcuts, screen capture, font enumeration, local-core
// install); the mobile Platform impl stubs the corresponding methods.
#[cfg(desktop)]
pub mod capture;
#[cfg(desktop)]
pub mod fonts;
#[cfg(desktop)]
pub mod pairing;
#[cfg(desktop)]
pub mod tray;
#[cfg(desktop)]
pub mod window;

#[cfg(desktop)]
pub use capture::{
    capture_monitor, capture_monitor_region, get_region_capture_target,
    hide_region_capture_overlay, list_capture_monitors, set_region_capture_target,
    show_region_capture_overlay,
};
pub use client_files::{
    delete_client_snippet, read_client_file, read_client_snippets, write_client_file,
    write_client_snippet,
};
pub use client_storage::{get_client_storage, truncate_client_log};
#[cfg(desktop)]
pub use fonts::list_system_fonts;
pub use keychain::{
    init_default_store, keychain_delete_token, keychain_get_token, keychain_set_token,
};
#[cfg(desktop)]
pub use net::discover_lan_cores;
pub use net::{net_fetch, net_ws_close, net_ws_open, net_ws_send};
#[cfg(desktop)]
pub use pairing::{
    local_core_base_url, local_core_installed, local_core_running, local_sidecar_ports,
    read_admin_token, read_launch_prefill, read_local_core_boot_error, start_local_core,
    stop_local_core, stop_spawned_core,
};
pub use process::get_self_metrics;
#[cfg(desktop)]
pub use process::was_autostarted;
#[cfg(desktop)]
pub use tray::{set_tray_core_state, NO_CORE_LABEL};
#[cfg(desktop)]
pub use window::{
    hide_main_window, position_window, request_hide_main_window, set_global_shortcut,
    set_input_shortcuts, set_ptt_config, show_main_window, toggle_main_window, validate_shortcut,
};

use crate::error::AppResult;
use tauri::AppHandle;
// `Manager` brings `handle.path()` into scope for tilde expansion + file
// conversion, both of which are desktop-only (android has no home dir and routes
// conversion through the core), so it is unused on android.
#[cfg(not(target_os = "android"))]
use tauri::Manager;

// -------------------------------------------------------------------
// System volume (desktop only: no app-level OS volume control on Android)
// -------------------------------------------------------------------

/// Read the default output device's master volume as a 0–100 percent value.
#[cfg(desktop)]
#[tauri::command]
pub async fn get_system_volume() -> AppResult<u8> {
    Ok(cpvc::get_system_volume())
}

/// Lower the system volume to `percent` of its current value for the duration
/// of an STT listening session, capturing the original level into
/// `state.saved_volume` so it can be restored later.
#[cfg(desktop)]
#[tauri::command]
pub async fn set_system_volume(
    state: tauri::State<'_, crate::state::AppState>,
    percent: u8,
) -> AppResult<()> {
    let percent = percent.min(100);
    let baseline: u8 = {
        let mut saved = state
            .0
            .saved_volume
            .lock()
            .map_err(|e| AppError::external(format!("saved_volume mutex poisoned: {e}")))?;
        match *saved {
            Some(v) => v,
            None => {
                let current = cpvc::get_system_volume();
                *saved = Some(current);
                current
            }
        }
    };
    let target = (((baseline as u32) * (percent as u32) + 50) / 100).min(100) as u8;
    if !cpvc::set_system_volume(target) {
        return Err(AppError::external(
            "Failed to set system volume via cpvc".to_string(),
        ));
    }
    Ok(())
}

/// Restore the previously-captured system volume (set by `set_system_volume`).
#[cfg(desktop)]
#[tauri::command]
pub async fn restore_system_volume(
    state: tauri::State<'_, crate::state::AppState>,
) -> AppResult<()> {
    let prev = {
        let mut saved = state
            .0
            .saved_volume
            .lock()
            .map_err(|e| AppError::external(format!("saved_volume mutex poisoned: {e}")))?;
        saved.take()
    };
    if let Some(v) = prev {
        if !cpvc::set_system_volume(v) {
            return Err(AppError::external(
                "Failed to restore system volume via cpvc".to_string(),
            ));
        }
    }
    Ok(())
}

// -------------------------------------------------------------------
// Path resolution
// -------------------------------------------------------------------

/// Pure tilde-expansion. `expand_tilde("~/foo", "/home/u") -> "/home/u/foo"`.
/// Non-`~` paths pass through unchanged.
pub fn expand_tilde(path: &str, home: &std::path::Path) -> String {
    if let Some(rest) = path.strip_prefix('~') {
        let rest = rest.trim_start_matches('/');
        home.join(rest).to_string_lossy().to_string()
    } else {
        std::path::Path::new(path).to_string_lossy().to_string()
    }
}

/// Expand a leading `~` in the given path to the user's home directory.
#[tauri::command]
pub fn resolve_path(handle: AppHandle, path: String) -> AppResult<String> {
    if !path.starts_with('~') {
        return Ok(expand_tilde(&path, std::path::Path::new("")));
    }
    // Tilde expansion needs a home directory. Android has none, so a `~` path is
    // left untouched there rather than erroring; on desktop it expands normally.
    #[cfg(target_os = "android")]
    {
        let _ = &handle;
        Ok(path)
    }
    #[cfg(not(target_os = "android"))]
    {
        let home = handle.path().home_dir()?;
        Ok(expand_tilde(&path, &home))
    }
}

/// The install channel baked into this build ("stable" | "dev" | "latest").
/// The Android self-host updater uses it to target the matching channel's
/// android.json instead of always reading stable's.
#[tauri::command]
pub fn client_channel() -> &'static str {
    crate::channel::channel()
}

/// Whether an in-app client update can be applied to this install in place.
///
/// The Tauri updater swaps the running bundle for macOS (.app) and Windows
/// (NSIS), but on Linux it only replaces an AppImage (the file named by the
/// `APPIMAGE` env var it was launched from). We ship the Linux client only as an
/// AppImage, so this is normally true; it returns false for any OTHER Linux
/// packaging (a distro/third-party repackage, or a raw binary), where an in-app
/// update would fail - the UI sends those users to the download page instead.
#[tauri::command]
pub fn can_self_install() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("APPIMAGE").is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

// -------------------------------------------------------------------
// File conversion (anytomd + pdf-extract). Desktop only: those crates are
// not built for Android; mobile attachments route through the core instead.
// -------------------------------------------------------------------

#[cfg(desktop)]
use crate::error::AppError;

#[cfg(desktop)]
const MAX_CONVERTIBLE_FILE_BYTES: u64 = 50 * 1024 * 1024;

#[cfg(desktop)]
const ALLOWED_CONVERTIBLE_EXTS: &[&str] = &[
    "docx", "pptx", "xlsx", "xls", "csv", "html", "htm", "txt", "md", "json", "xml", "rst", "log",
    "toml", "yaml", "ini", "py", "rs", "js", "ts", "c", "cpp", "go", "java", "pdf",
];

/// Validate `path` is small enough and its extension is in the allow-list.
/// Returns the lowercased extension (which the caller uses to pick the
/// pdf-extract vs anytomd branch).
#[cfg(desktop)]
pub fn validate_convertible_file(path: &std::path::Path, size_bytes: u64) -> AppResult<String> {
    if size_bytes > MAX_CONVERTIBLE_FILE_BYTES {
        return Err(AppError::validation(format!(
            "File too large ({} bytes, max 50MB)",
            size_bytes
        )));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    if !ALLOWED_CONVERTIBLE_EXTS.contains(&ext.as_str()) {
        return Err(AppError::validation(format!(
            "Unsupported file type: .{ext}"
        )));
    }
    Ok(ext)
}

/// Convert the file at `file_path` to Markdown for attachment as document
/// context. Kept client-side so the rich Rust crate ecosystem stays
/// available; the client POSTs the resulting markdown to core.
#[cfg(desktop)]
#[tauri::command]
pub async fn convert_file_to_markdown(file_path: String) -> AppResult<String> {
    let canonical = tokio::fs::canonicalize(&file_path).await?;
    let meta = tokio::fs::metadata(&canonical).await?;
    let ext = validate_convertible_file(&canonical, meta.len())?;
    if ext == "pdf" {
        let text = pdf_extract::extract_text(&canonical)
            .map_err(|e| AppError::external(format!("Failed to extract PDF text: {e}")))?;
        return Ok(text);
    }
    let options = anytomd::ConversionOptions::default();
    let result = anytomd::convert_file(&canonical, &options)
        .map_err(|e| AppError::external(e.to_string()))?;
    Ok(result.markdown)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// `expand_tilde` joins with the host path separator (`\` on Windows),
    /// which is correct at runtime but breaks these forward-slash string
    /// comparisons on Windows. Normalize so the assertions hold on every
    /// platform.
    fn norm(s: String) -> String {
        s.replace('\\', "/")
    }

    #[test]
    fn expand_tilde_replaces_leading_tilde_with_home() {
        assert_eq!(
            norm(expand_tilde("~/foo/bar", &PathBuf::from("/home/u"))),
            "/home/u/foo/bar"
        );
    }

    #[test]
    fn expand_tilde_handles_bare_tilde() {
        // PathBuf::join("") preserves the trailing separator on Unix; the
        // result is still the same directory.
        let out = norm(expand_tilde("~", &PathBuf::from("/home/u")));
        assert!(out == "/home/u" || out == "/home/u/", "got {out}");
    }

    #[test]
    fn expand_tilde_handles_tilde_without_slash() {
        // `~foo` is treated as `<home>/foo` (matches the original logic which
        // just trims any leading slashes from the remainder).
        assert_eq!(
            norm(expand_tilde("~foo", &PathBuf::from("/home/u"))),
            "/home/u/foo"
        );
    }

    #[test]
    fn expand_tilde_passes_through_absolute_paths() {
        assert_eq!(
            expand_tilde("/etc/hosts", &PathBuf::from("/home/u")),
            "/etc/hosts"
        );
    }

    #[test]
    fn expand_tilde_passes_through_relative_paths() {
        assert_eq!(
            expand_tilde("foo/bar", &PathBuf::from("/home/u")),
            "foo/bar"
        );
    }

    #[test]
    fn validate_convertible_file_accepts_known_extension() {
        let ext = validate_convertible_file(std::path::Path::new("/tmp/note.md"), 1024).unwrap();
        assert_eq!(ext, "md");
    }

    #[test]
    fn validate_convertible_file_is_case_insensitive() {
        let ext =
            validate_convertible_file(std::path::Path::new("/tmp/REPORT.DOCX"), 1024).unwrap();
        assert_eq!(ext, "docx");
    }

    #[test]
    fn validate_convertible_file_rejects_unknown_extension() {
        let err =
            validate_convertible_file(std::path::Path::new("/tmp/binary.exe"), 1024).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Unsupported file type"));
        assert!(msg.contains(".exe"));
    }

    #[test]
    fn validate_convertible_file_rejects_files_without_extension() {
        let err =
            validate_convertible_file(std::path::Path::new("/tmp/Makefile"), 1024).unwrap_err();
        assert!(format!("{err}").contains("Unsupported file type"));
    }

    #[test]
    fn validate_convertible_file_rejects_over_50mb() {
        let err = validate_convertible_file(
            std::path::Path::new("/tmp/huge.md"),
            MAX_CONVERTIBLE_FILE_BYTES + 1,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("File too large"));
    }

    #[test]
    fn validate_convertible_file_accepts_exactly_50mb() {
        // Boundary: exactly the max is fine (> is the failure condition).
        let ext = validate_convertible_file(
            std::path::Path::new("/tmp/edge.md"),
            MAX_CONVERTIBLE_FILE_BYTES,
        )
        .unwrap();
        assert_eq!(ext, "md");
    }

    #[test]
    fn validate_convertible_file_pdf_is_in_allowlist() {
        let ext = validate_convertible_file(std::path::Path::new("/tmp/x.pdf"), 100).unwrap();
        assert_eq!(ext, "pdf");
    }
}
