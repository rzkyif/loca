//! System font enumeration. Backs the `select` field type's
//! `optionsSource: "fonts"` in the settings UI.

use crate::error::{AppError, AppResult};
use font_kit::source::SystemSource;

/// Return all installed font family names, sorted and de-duplicated.
#[tauri::command]
pub async fn list_system_fonts() -> AppResult<Vec<String>> {
    let mut families = SystemSource::new()
        .all_families()
        .map_err(|e| AppError::external(e.to_string()))?;
    families.sort_unstable();
    families.dedup();
    Ok(families)
}
