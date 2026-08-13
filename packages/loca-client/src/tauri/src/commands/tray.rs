//! Tray-icon state pushed from the webview.
//!
//! The tray menu's core item mirrors the local core the webview already tracks
//! (installed on disk? port answering?), so the webview pushes that state here
//! rather than Rust polling the port on a timer. The item itself only ever EMITS
//! an intent (`tray-core-start` / `tray-core-stop`); the webview performs the
//! action, so the tray and the CoreBar pill share one code path.

use crate::error::AppResult;
use crate::state::AppState;
use std::sync::atomic::Ordering;
use tauri::State;

/// The three labels the one core item carries. A Tauri 2 menu item cannot be
/// hidden (muda exposes `set_enabled` / `set_text`, not visibility), so with no
/// core on this device the item stays put and says so, disabled, rather than
/// offering a Start that could never work.
pub const STOP_CORE_LABEL: &str = "Stop Core";
pub const START_CORE_LABEL: &str = "Start Core";
pub const NO_CORE_LABEL: &str = "Core Not Installed";

/// The label for a given local-core state. `installed` decides whether the item
/// is an action at all; `running` picks which action it is.
fn core_item_label(installed: bool, running: bool) -> &'static str {
    match (installed, running) {
        (false, _) => NO_CORE_LABEL,
        (true, true) => STOP_CORE_LABEL,
        (true, false) => START_CORE_LABEL,
    }
}

/// Reflect the local core's state in the tray menu: `installed` enables the item
/// and, with `running`, picks its label and the intent a click emits.
#[tauri::command]
pub fn set_tray_core_state(
    installed: bool,
    running: bool,
    state: State<'_, AppState>,
) -> AppResult<()> {
    state.0.tray_core_running.store(running, Ordering::SeqCst);
    if let Ok(item) = state.0.tray_core_item.lock() {
        if let Some(item) = item.as_ref() {
            item.set_enabled(installed)?;
            item.set_text(core_item_label(installed, running))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_core_names_itself_rather_than_offering_a_dead_start() {
        assert_eq!(core_item_label(false, false), NO_CORE_LABEL);
        // Nothing is installed, so a stale `running` must not resurrect "Stop Core".
        assert_eq!(core_item_label(false, true), NO_CORE_LABEL);
    }

    #[test]
    fn an_installed_core_offers_the_action_it_is_missing() {
        assert_eq!(core_item_label(true, false), START_CORE_LABEL);
        assert_eq!(core_item_label(true, true), STOP_CORE_LABEL);
    }
}
