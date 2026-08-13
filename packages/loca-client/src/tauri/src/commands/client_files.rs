// Client-side JSON stores under ~/.loca/<channel>/client/, one file per
// concern so no two modules ever read-modify-write the same file:
//   settings.json : sparse user settings (frontend schema owns ids/defaults)
//   cores.json    : paired-cores registry ({ cores, currentCoreId })
//   snippets/     : one <name>.json per snippet; the directory listing IS the
//                   registry, so users can share a snippet by copying its file
//                   in and rescanning (or restarting).
//
// The Tauri commands delegate to plain functions that take explicit paths so
// unit tests can drive them against a tempdir instead of the real home
// directory.

use crate::error::{AppError, AppResult};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tauri::AppHandle;

/// The fixed-name client JSON files. The enum is the allowlist: the frontend
/// can only ever address these, never an arbitrary path.
#[derive(serde::Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ClientFile {
    Settings,
    Cores,
}

impl ClientFile {
    fn filename(self) -> &'static str {
        match self {
            ClientFile::Settings => "settings.json",
            ClientFile::Cores => "cores.json",
        }
    }
}

#[tauri::command]
pub fn read_client_file(handle: AppHandle, file: ClientFile) -> AppResult<Value> {
    read_json_at(&client_root(&handle)?.join(file.filename()))
}

#[tauri::command]
pub fn write_client_file(handle: AppHandle, file: ClientFile, data: Value) -> AppResult<()> {
    write_json_at(&client_root(&handle)?.join(file.filename()), data)
}

// --- snippets ---------------------------------------------------------------

/// Read every parseable `<name>.json` under the snippets dir, keyed by the
/// filename stem. Non-JSON files, files with unusable names, and unparseable
/// content are skipped (with a log) rather than failing the whole read: a
/// user-dropped file must never brick the snippet list.
#[tauri::command]
pub fn read_client_snippets(handle: AppHandle) -> AppResult<serde_json::Map<String, Value>> {
    read_snippets_in(&snippets_dir(&handle)?)
}

#[tauri::command]
pub fn write_client_snippet(handle: AppHandle, name: String, data: Value) -> AppResult<()> {
    validate_snippet_name(&name)?;
    write_json_at(&snippets_dir(&handle)?.join(format!("{name}.json")), data)
}

/// Remove a snippet file. NotFound-tolerant: deleting an already-gone snippet
/// is success.
#[tauri::command]
pub fn delete_client_snippet(handle: AppHandle, name: String) -> AppResult<()> {
    validate_snippet_name(&name)?;
    match std::fs::remove_file(snippets_dir(&handle)?.join(format!("{name}.json"))) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(AppError::Io(e)),
    }
}

pub fn read_snippets_in(dir: &Path) -> AppResult<serde_json::Map<String, Value>> {
    let mut out = serde_json::Map::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(AppError::Io(e)),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if validate_snippet_name(stem).is_err() {
            log::warn!(target: "loca::snippets", "skipping snippet with unusable name: {}", path.display());
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());
        match parsed {
            Some(v) if v.is_object() => {
                out.insert(stem.to_string(), v);
            }
            _ => {
                log::warn!(target: "loca::snippets", "skipping unparseable snippet file: {}", path.display());
            }
        }
    }
    Ok(out)
}

/// Filename-stem rules for snippet files. Deliberately a denylist so shared
/// files with human names ("My Snippet.json") keep working; only what can
/// escape the directory or hide the file is rejected.
pub fn validate_snippet_name(name: &str) -> AppResult<()> {
    if name.is_empty() {
        return Err(AppError::validation("snippet name is empty"));
    }
    if name.len() > 128 {
        return Err(AppError::validation("snippet name exceeds 128 chars"));
    }
    if name.starts_with('.') {
        return Err(AppError::validation(
            "snippet name must not start with a dot",
        ));
    }
    if name
        .chars()
        .any(|c| c == '/' || c == '\\' || c.is_control())
    {
        return Err(AppError::validation(
            "snippet name has disallowed characters",
        ));
    }
    Ok(())
}

// --- shared internals --------------------------------------------------------

pub fn read_json_at(path: &Path) -> AppResult<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(AppError::from),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(Value::Object(Default::default()))
        }
        Err(err) => Err(AppError::Io(err)),
    }
}

pub fn write_json_at(path: &Path, data: Value) -> AppResult<()> {
    if !data.is_object() {
        return Err(AppError::validation("data must be an object"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&data)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

use super::paths::client_root;

fn snippets_dir(handle: &AppHandle) -> AppResult<PathBuf> {
    Ok(client_root(handle)?.join("snippets"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::env;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn unique_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        env::temp_dir().join(format!("loca-client-files-{}-{}", std::process::id(), n))
    }

    #[test]
    fn client_file_maps_to_expected_filenames() {
        assert_eq!(ClientFile::Settings.filename(), "settings.json");
        assert_eq!(ClientFile::Cores.filename(), "cores.json");
    }

    #[test]
    fn read_missing_file_returns_empty_object() {
        let path = unique_dir().join("settings.json");
        let val = read_json_at(&path).unwrap();
        assert_eq!(val, json!({}));
    }

    #[test]
    fn write_then_read_round_trips_object() {
        let path = unique_dir().join("settings.json");
        write_json_at(&path, json!({ "theme": "dark", "fontSize": 14 })).unwrap();
        let val = read_json_at(&path).unwrap();
        assert_eq!(val["theme"], "dark");
        assert_eq!(val["fontSize"], 14);
    }

    #[test]
    fn write_rejects_non_object_values() {
        let path = unique_dir().join("settings.json");
        let err = write_json_at(&path, json!([1, 2, 3])).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[test]
    fn write_creates_parent_directories() {
        let path = unique_dir().join("nested/deeper/settings.json");
        write_json_at(&path, json!({ "k": "v" })).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn read_propagates_invalid_json_as_app_error() {
        let path = unique_dir().join("settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();
        let err = read_json_at(&path).unwrap_err();
        assert!(matches!(err, AppError::Serde(_)));
    }

    #[test]
    fn snippets_read_missing_dir_returns_empty_map() {
        let map = read_snippets_in(&unique_dir()).unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn snippets_read_keys_by_filename_stem() {
        let dir = unique_dir();
        write_json_at(&dir.join("greet.json"), json!({ "trigger": "@hi" })).unwrap();
        write_json_at(&dir.join("My Snippet.json"), json!({ "trigger": "@my" })).unwrap();
        let map = read_snippets_in(&dir).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map["greet"]["trigger"], "@hi");
        assert_eq!(map["My Snippet"]["trigger"], "@my");
    }

    #[test]
    fn snippets_read_skips_bad_files() {
        let dir = unique_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_json_at(&dir.join("good.json"), json!({ "trigger": "@ok" })).unwrap();
        std::fs::write(dir.join("broken.json"), b"{ nope").unwrap();
        std::fs::write(dir.join("array.json"), b"[1,2]").unwrap();
        std::fs::write(dir.join("notes.txt"), b"not a snippet").unwrap();
        std::fs::write(dir.join(".hidden.json"), b"{}").unwrap();
        let map = read_snippets_in(&dir).unwrap();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("good"));
    }

    #[test]
    fn validate_snippet_name_blocks_traversal_and_hidden_files() {
        assert!(validate_snippet_name("").is_err());
        assert!(validate_snippet_name(&"a".repeat(129)).is_err());
        assert!(validate_snippet_name(".hidden").is_err());
        assert!(validate_snippet_name("..").is_err());
        assert!(validate_snippet_name("a/b").is_err());
        assert!(validate_snippet_name("a\\b").is_err());
        assert!(validate_snippet_name("a\nb").is_err());
        assert!(validate_snippet_name("greet").is_ok());
        assert!(validate_snippet_name("My Snippet 2").is_ok());
    }
}
