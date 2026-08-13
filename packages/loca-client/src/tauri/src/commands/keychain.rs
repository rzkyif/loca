// OS-keychain wrapper for paired-core bearer tokens.
//
// Service: channel-namespaced ("loca-client" on stable, "loca-client-dev" /
// "loca-client-latest" otherwise, see `crate::channel`), so a dev/latest build's
// tokens never collide with a stable install's. Account format: "core:<coreId>",
// where <coreId> is the ULID assigned by the core during pairing-claim.
//
// Backing store, chosen per platform + channel by `store()`:
//   - desktop stable / latest (signed bundles): the real OS keychain via
//     keyring-core and the platform credential store (`RealKeychain`),
//     encrypted at rest.
//   - desktop dev (`deno task dev`, an unsigned `tauri dev` binary): a file
//     under the channel-isolated client dir (`FileKeychain`). The macOS keychain
//     silently no-ops for the unsigned dev build: `set_password` returns Ok
//     but the entry never persists, so a freshly paired token is gone by the
//     time `select()` reads it back ("no token for core … re-pair"). Mirrors
//     loca-core's `.master-key` file fallback for the same class of reason.
//   - android (all channels): the same `FileKeychain`, since no keyring-core OS
//     backend is wired for android. The token is stored as plaintext JSON in the
//     app-private data dir. This is NOT encrypted at rest like the desktop OS
//     keychain; it relies on the per-app sandbox plus `allowBackup="false"` in
//     the manifest (so backups can't copy it off-device). A future improvement
//     is an Android Keystore / EncryptedSharedPreferences backend.
//
// The `KeychainStore` trait is the test seam: unit tests swap in
// `InMemoryKeychain` and drive `FileKeychain` via tempdir paths, so they
// exercise the shared `set_token`/`get_token`/`delete_token` + `account()`
// plumbing without touching the real OS keychain.

#[cfg(not(target_os = "android"))]
use crate::channel::channel;
use crate::channel::keychain_service;
use crate::error::{AppError, AppResult};
use keyring_core::Entry;
use std::collections::HashMap;
use std::path::PathBuf;
use tauri::AppHandle;

// Input limits applied at the Tauri command boundary. Real values are far
// smaller (ULID core_id = 26 chars; bearer token = 43 chars base64url),
// but capping defends against a misbehaving caller pinning memory or
// exhausting the OS keychain via a giant write.
const MAX_CORE_ID_LEN: usize = 64;
const MAX_TOKEN_LEN: usize = 256;

pub trait KeychainStore: Send + Sync {
    fn set(&self, account: &str, token: &str) -> AppResult<()>;
    fn get(&self, account: &str) -> AppResult<Option<String>>;
    fn delete(&self, account: &str) -> AppResult<()>;
}

pub struct RealKeychain;

impl KeychainStore for RealKeychain {
    fn set(&self, account: &str, token: &str) -> AppResult<()> {
        let entry = Entry::new(&keychain_service(), account)?;
        entry.set_password(token).map_err(classify_keyring_error)?;
        Ok(())
    }
    fn get(&self, account: &str) -> AppResult<Option<String>> {
        let entry = Entry::new(&keychain_service(), account)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(err) => Err(classify_keyring_error(err)),
        }
    }
    fn delete(&self, account: &str) -> AppResult<()> {
        let entry = Entry::new(&keychain_service(), account)?;
        match entry.delete_credential() {
            Ok(_) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(err) => Err(classify_keyring_error(err)),
        }
    }
}

/// Register the platform credential store as keyring-core's process-global
/// default so `RealKeychain` entries resolve. Called once at client startup
/// (see `run()`); the dev channel uses the file-backed store instead, so a
/// failure here is non-fatal and only logged.
pub fn init_default_store() -> AppResult<()> {
    #[cfg(target_os = "macos")]
    {
        let store = apple_native_keyring_store::keychain::Store::new()?;
        keyring_core::set_default_store(store);
    }
    #[cfg(target_os = "ios")]
    {
        // iOS exposes only the "protected data" keychain (see Cargo.toml); the
        // legacy `keychain` module is macOS-only.
        let store = apple_native_keyring_store::protected::Store::new()?;
        keyring_core::set_default_store(store);
    }
    #[cfg(target_os = "windows")]
    {
        let store = windows_native_keyring_store::Store::new()?;
        keyring_core::set_default_store(store);
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
    {
        let store = dbus_secret_service_keyring_store::Store::new()?;
        keyring_core::set_default_store(store);
    }
    // Android has no keyring-core OS backend wired here: tokens go to the
    // app-private file store selected in `store()`, so there is nothing to
    // register as the process-global default.
    Ok(())
}

// --- file-backed store (desktop dev + android) ----------------------------

/// File-backed keychain: the desktop `dev` build's fallback AND android's only
/// store (see the module header). Persists the `{ "core:<id>": "<token>" }` map
/// as pretty JSON under the channel-isolated client dir. Plaintext at rest, so
/// it leans on the OS sandbox; holds its own path so tests can drive it against
/// a tempdir instead of the real data directory.
struct FileKeychain {
    path: PathBuf,
}

impl FileKeychain {
    fn read_map(&self) -> AppResult<HashMap<String, String>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).map_err(AppError::from),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(err) => Err(AppError::Io(err)),
        }
    }

    fn write_map(&self, map: &HashMap<String, String>) -> AppResult<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(map)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl KeychainStore for FileKeychain {
    fn set(&self, account: &str, token: &str) -> AppResult<()> {
        let mut map = self.read_map()?;
        map.insert(account.to_string(), token.to_string());
        self.write_map(&map)
    }
    fn get(&self, account: &str) -> AppResult<Option<String>> {
        Ok(self.read_map()?.get(account).cloned())
    }
    fn delete(&self, account: &str) -> AppResult<()> {
        let mut map = self.read_map()?;
        if map.remove(account).is_some() {
            self.write_map(&map)?;
        }
        Ok(())
    }
}

/// `<client-data-root>/keychain.json` is the file-backed token store, alongside
/// the client's `settings.json`. The root is per-platform (home-based on
/// desktop, the app-private data dir on android); see commands::paths.
fn keychain_file_path(handle: &AppHandle) -> AppResult<PathBuf> {
    Ok(super::paths::client_root(handle)?.join("keychain.json"))
}

/// Pick the keychain backing for this platform + channel. Android has no
/// keyring-core OS backend wired, so it always uses the app-private file store.
/// On desktop, the unsigned `dev` build can't use the macOS keychain (writes
/// report success but never persist), so it stores tokens in a file; the signed
/// stable/latest bundles use the real OS keychain.
fn store(handle: &AppHandle) -> AppResult<Box<dyn KeychainStore>> {
    #[cfg(target_os = "android")]
    {
        return Ok(Box::new(FileKeychain {
            path: keychain_file_path(handle)?,
        }));
    }
    #[cfg(not(target_os = "android"))]
    {
        if channel() == "dev" {
            Ok(Box::new(FileKeychain {
                path: keychain_file_path(handle)?,
            }))
        } else {
            let _ = handle;
            Ok(Box::new(RealKeychain))
        }
    }
}

/// Map a raw `keyring_core::Error` into one of three generic codes so the UI
/// (and any log line that captures the error) never sees platform-specific
/// detail like exact file paths, COM HRESULTs, or DBus method names. Those
/// can be useful for debugging but leak host topology to anyone who can
/// read the client logs.
fn classify_keyring_error(err: keyring_core::Error) -> AppError {
    use keyring_core::Error as E;
    let code = match err {
        E::NoEntry => "keychain:not_found",
        // PlatformFailure / NoStorageAccess on macOS map to the user
        // refusing the keychain prompt or the daemon being unreachable.
        E::PlatformFailure(_) | E::NoStorageAccess(_) => "keychain:unavailable",
        // BadEncoding / TooLong / Invalid are caller-input mistakes we
        // already cap before calling; surface as denied so the user knows
        // it's not their cert/keyring config.
        E::BadEncoding(_) | E::TooLong(_, _) | E::Invalid(_, _) => "keychain:denied",
        _ => "keychain:unavailable",
    };
    AppError::External(code.to_string())
}

#[tauri::command]
pub fn keychain_set_token(handle: AppHandle, core_id: String, token: String) -> AppResult<()> {
    validate_core_id(&core_id)?;
    validate_token(&token)?;
    set_token(&*store(&handle)?, &core_id, &token)
}

#[tauri::command]
pub fn keychain_get_token(handle: AppHandle, core_id: String) -> AppResult<Option<String>> {
    validate_core_id(&core_id)?;
    get_token(&*store(&handle)?, &core_id)
}

#[tauri::command]
pub fn keychain_delete_token(handle: AppHandle, core_id: String) -> AppResult<()> {
    validate_core_id(&core_id)?;
    delete_token(&*store(&handle)?, &core_id)
}

fn validate_core_id(id: &str) -> AppResult<()> {
    if id.is_empty() {
        return Err(AppError::validation("core_id is empty"));
    }
    if id.len() > MAX_CORE_ID_LEN {
        return Err(AppError::validation(format!(
            "core_id exceeds {} chars",
            MAX_CORE_ID_LEN
        )));
    }
    // Restrict to URL-safe ULID-shaped chars so account strings can never
    // contain keychain delimiters that the platform might interpret oddly.
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AppError::validation("core_id has disallowed characters"));
    }
    Ok(())
}

fn validate_token(token: &str) -> AppResult<()> {
    if token.is_empty() {
        return Err(AppError::validation("token is empty"));
    }
    if token.len() > MAX_TOKEN_LEN {
        return Err(AppError::validation(format!(
            "token exceeds {} chars",
            MAX_TOKEN_LEN
        )));
    }
    Ok(())
}

pub fn set_token<S: KeychainStore + ?Sized>(
    store: &S,
    core_id: &str,
    token: &str,
) -> AppResult<()> {
    store.set(&account(core_id), token)
}

pub fn get_token<S: KeychainStore + ?Sized>(store: &S, core_id: &str) -> AppResult<Option<String>> {
    store.get(&account(core_id))
}

pub fn delete_token<S: KeychainStore + ?Sized>(store: &S, core_id: &str) -> AppResult<()> {
    store.delete(&account(core_id))
}

fn account(core_id: &str) -> String {
    format!("core:{}", core_id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct InMemoryKeychain {
        inner: Mutex<HashMap<String, String>>,
    }

    impl KeychainStore for InMemoryKeychain {
        fn set(&self, account: &str, token: &str) -> AppResult<()> {
            self.inner
                .lock()
                .unwrap()
                .insert(account.to_string(), token.to_string());
            Ok(())
        }
        fn get(&self, account: &str) -> AppResult<Option<String>> {
            Ok(self.inner.lock().unwrap().get(account).cloned())
        }
        fn delete(&self, account: &str) -> AppResult<()> {
            self.inner.lock().unwrap().remove(account);
            Ok(())
        }
    }

    #[test]
    fn account_uses_core_prefix() {
        assert_eq!(account("01H8XGJWBWBAQ4WG"), "core:01H8XGJWBWBAQ4WG");
    }

    #[test]
    fn set_then_get_round_trips() {
        let store = InMemoryKeychain::default();
        set_token(&store, "abc", "bearer-xyz").unwrap();
        assert_eq!(get_token(&store, "abc").unwrap(), Some("bearer-xyz".into()));
    }

    #[test]
    fn get_returns_none_for_missing_entry() {
        let store = InMemoryKeychain::default();
        assert_eq!(get_token(&store, "missing").unwrap(), None);
    }

    #[test]
    fn delete_removes_entry_and_is_idempotent() {
        let store = InMemoryKeychain::default();
        set_token(&store, "abc", "t").unwrap();
        delete_token(&store, "abc").unwrap();
        delete_token(&store, "abc").unwrap();
        assert_eq!(get_token(&store, "abc").unwrap(), None);
    }

    #[test]
    fn validate_core_id_rejects_empty_oversized_and_disallowed_chars() {
        assert!(validate_core_id("").is_err());
        assert!(validate_core_id(&"a".repeat(MAX_CORE_ID_LEN + 1)).is_err());
        assert!(validate_core_id("with space").is_err());
        assert!(validate_core_id("with/slash").is_err());
        assert!(validate_core_id("01H8XGJWBWBAQ4WG-ok_too").is_ok());
    }

    #[test]
    fn validate_token_rejects_empty_and_oversized() {
        assert!(validate_token("").is_err());
        assert!(validate_token(&"x".repeat(MAX_TOKEN_LEN + 1)).is_err());
        // Tokens may contain '=' and other base64url padding; we only
        // gate on length, not character class.
        assert!(validate_token("abc==").is_ok());
    }

    #[test]
    fn classify_keyring_error_maps_to_generic_codes() {
        // NoEntry → not_found
        let e = classify_keyring_error(keyring_core::Error::NoEntry);
        assert_eq!(format!("{}", e), "keychain:not_found");
        // BadEncoding → denied (caller-input mistake)
        let e = classify_keyring_error(keyring_core::Error::BadEncoding(vec![0xff]));
        assert_eq!(format!("{}", e), "keychain:denied");
        // TooLong → denied
        let e = classify_keyring_error(keyring_core::Error::TooLong("svc".into(), 10));
        assert_eq!(format!("{}", e), "keychain:denied");
    }

    // --- dev file fallback ---

    fn unique_keychain_path() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("loca-client-keychain-{}-{}", std::process::id(), n))
            .join("keychain.json")
    }

    #[test]
    fn dev_file_keychain_round_trips_and_deletes() {
        let store = FileKeychain {
            path: unique_keychain_path(),
        };
        set_token(&store, "01H8XGJWBWBAQ4WG", "bearer-xyz").unwrap();
        assert_eq!(
            get_token(&store, "01H8XGJWBWBAQ4WG").unwrap(),
            Some("bearer-xyz".into())
        );
        delete_token(&store, "01H8XGJWBWBAQ4WG").unwrap();
        assert_eq!(get_token(&store, "01H8XGJWBWBAQ4WG").unwrap(), None);
    }

    #[test]
    fn dev_file_keychain_persists_across_instances() {
        // The exact bug this fixes: addPaired()'s set and select()'s get may
        // run through different store instances; the token must survive on disk.
        let path = unique_keychain_path();
        set_token(&FileKeychain { path: path.clone() }, "core-1", "tok").unwrap();
        assert_eq!(
            get_token(&FileKeychain { path }, "core-1").unwrap(),
            Some("tok".into())
        );
    }

    #[test]
    fn dev_file_keychain_get_missing_returns_none() {
        let store = FileKeychain {
            path: unique_keychain_path(),
        };
        assert_eq!(get_token(&store, "absent").unwrap(), None);
    }
}
