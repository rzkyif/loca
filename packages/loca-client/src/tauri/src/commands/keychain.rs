// OS-keychain wrapper for paired-core bearer tokens. Service is
// channel-namespaced (see `crate::channel`); accounts are "core:<coreId>".
//
// Backing store, chosen per platform + channel by `store()`:
//   - desktop stable / latest: the real OS keychain (`RealKeychain`).
//   - desktop dev: a file under the channel-isolated client dir
//     (`FileKeychain`). The macOS keychain silently no-ops for the unsigned dev
//     build: `set_password` returns Ok but the entry never persists, so a
//     freshly paired token is gone by the time `select()` reads it back.
//   - android: `SealedFileKeychain`, that same file sealed under an
//     AES-256-GCM key held in the AndroidKeyStore (`SecretBox.kt`, over JNI).
//
// `KeychainStore` is the test seam: tests swap in `InMemoryKeychain` and drive
// the file stores against tempdirs.

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
/// default so `RealKeychain` entries resolve. Called once at startup; the dev
/// channel uses the file store instead, so a failure here is only logged.
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
    // Android has no keyring-core backend: `store()` picks the sealed file.
    Ok(())
}

// --- file-backed store (desktop dev + android) ----------------------------

/// Persists the `{ "core:<id>": "<token>" }` map as pretty JSON. Plaintext at
/// rest, so it leans on the OS sandbox. Holds its own path so tests can point
/// it at a tempdir.
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

/// `<client-data-root>/keychain.json`, alongside the client's `settings.json`.
fn keychain_file_path(handle: &AppHandle) -> AppResult<PathBuf> {
    Ok(super::paths::client_root(handle)?.join("keychain.json"))
}

// --- android: sealed under an AndroidKeyStore key --------------------------

/// The Kotlin half, pulled in so that deleting it fails the build. It is a JNI
/// callee, so nothing in Rust would otherwise reference it; `tauri android init`
/// regenerates the tree around it and a silent loss would drop this store back
/// to plaintext. The tripwire test below checks its contents on the host.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const SECRET_BOX_KT: &str =
    include_str!("../../gen/android/app/src/main/java/com/rzkyif/loca/SecretBox.kt");

/// The class the JNI layer loads, and the two methods it calls. Declared once so
/// the tripwire test and the call sites cannot drift.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const SECRET_BOX_CLASS: &str = "com.rzkyif.loca.SecretBox";
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const SECRET_BOX_SEAL: &str = "seal";
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const SECRET_BOX_UNSEAL: &str = "unseal";

#[cfg(target_os = "android")]
mod keystore {
    use super::{SECRET_BOX_CLASS, SECRET_BOX_SEAL, SECRET_BOX_UNSEAL};
    use loca_core_ffi::loca_core::error::{internal, Result as CoreResult};
    use loca_core_ffi::loca_core::secrets::android::SecretBox;

    /// Seals with an AES-256-GCM key the AndroidKeyStore holds and this process
    /// cannot extract. One JNI hop per call, into `SecretBox.kt`.
    pub struct KeystoreBox {
        alias: String,
    }

    impl KeystoreBox {
        pub fn new(alias: impl Into<String>) -> Self {
            Self {
                alias: alias.into(),
            }
        }

        /// Call one static `(String, byte[]) -> byte[]` method on the Kotlin
        /// object.
        ///
        /// The class is resolved through the Activity's classloader, NOT with
        /// `FindClass`: a `FindClass` from a Rust-created thread searches the
        /// system classloader, which cannot see application classes, so every
        /// off-UI-thread call (all of them) would fail. Nothing is cached;
        /// sealing happens a handful of times per boot.
        fn call(&self, method: &str, payload: &[u8]) -> CoreResult<Vec<u8>> {
            let ctx = ndk_context::android_context();
            // SAFETY: both raw pointers come from the runtime that started this
            // process and are valid for its lifetime. `android_context` is the
            // documented way to reach them.
            let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }
                .map_err(|err| internal(format!("no JavaVM: {err}")))?;
            let mut env = vm
                .attach_current_thread()
                .map_err(|err| internal(format!("could not attach to the JVM: {err}")))?;
            let context = unsafe { jni::objects::JObject::from_raw(ctx.context().cast()) };

            let loader = env
                .call_method(&context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
                .and_then(|v| v.l())
                .map_err(|err| internal(format!("no classloader: {err}")))?;
            let class_name = env
                .new_string(SECRET_BOX_CLASS)
                .map_err(|err| internal(format!("jni string: {err}")))?;
            let class = env
                .call_method(
                    &loader,
                    "loadClass",
                    "(Ljava/lang/String;)Ljava/lang/Class;",
                    &[(&class_name).into()],
                )
                .and_then(|v| v.l())
                .map_err(|err| internal(format!("{SECRET_BOX_CLASS} not loadable: {err}")))?;

            let alias = env
                .new_string(&self.alias)
                .map_err(|err| internal(format!("jni string: {err}")))?;
            let bytes = env
                .byte_array_from_slice(payload)
                .map_err(|err| internal(format!("jni byte array: {err}")))?;

            let out = env
                .call_static_method(
                    jni::objects::JClass::from(class),
                    method,
                    "(Ljava/lang/String;[B)[B",
                    &[(&alias).into(), (&bytes).into()],
                )
                .and_then(|v| v.l());

            // A Java exception stays pending on the env after a failed call and
            // would poison the next JNI call on this thread. Clear it, and keep
            // its description out of the message: an exception raised inside the
            // cipher is where a buffer would leak into a log.
            let out = match out {
                Ok(obj) => obj,
                Err(err) => {
                    let _ = env.exception_clear();
                    return Err(internal(format!("SecretBox.{method} failed: {err}")));
                }
            };
            if out.is_null() {
                return Err(internal(format!("SecretBox.{method} returned null")));
            }
            env.convert_byte_array(jni::objects::JByteArray::from(out))
                .map_err(|err| internal(format!("SecretBox.{method} result: {err}")))
        }
    }

    impl std::fmt::Debug for KeystoreBox {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("KeystoreBox")
        }
    }

    impl SecretBox for KeystoreBox {
        fn seal(&self, plaintext: &[u8]) -> CoreResult<Vec<u8>> {
            self.call(SECRET_BOX_SEAL, plaintext)
        }
        fn unseal(&self, sealed: &[u8]) -> CoreResult<Vec<u8>> {
            self.call(SECRET_BOX_UNSEAL, sealed)
        }
    }
}

#[cfg(target_os = "android")]
pub use keystore::KeystoreBox;

/// The KeyStore alias for the Core's vault master key, channel-namespaced the
/// same way `keychain_service()` already namespaces the client's. A separate
/// alias from the client's so that clearing one store cannot silently take the
/// other's key with it.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn core_keystore_alias() -> String {
    format!("{}-core", keychain_service())
}

/// The token map, sealed. Same path and JSON shape as [`FileKeychain`], so the
/// seam is the bytes on disk. The seal primitive is injected because the real
/// one is JNI and unreachable from a host test binary.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
struct SealedFileKeychain<B> {
    path: PathBuf,
    boxed: B,
}

/// The Core's framing, reused so both of this device's secret stores share one
/// on-disk format rather than two that can disagree about a version marker.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const SEALED_MAGIC: &str = "LOCAK1";
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const SEALED_PREFIX: &str = "LOCAK1:";

/// The version marker of a framed value, or `None` when it carries none.
///
/// A byte-for-byte mirror of `loca_core::secrets::android::marker_of`, which
/// cannot be shared: `loca-core-ffi` is only a dependency under
/// `cfg(any(android, ios))` and these tests run on the host. The copies are kept
/// in step by `the_two_stores_classify_the_same_bytes_the_same_way`, which
/// exists in BOTH suites over the same input table.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn marker_of(text: &str) -> Option<&str> {
    let idx = text.find(':')?;
    let marker = &text[..idx];
    let ok = !marker.is_empty()
        && marker
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    ok.then_some(marker)
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
impl<B: SealBytes> SealedFileKeychain<B> {
    fn read_map(&self) -> AppResult<HashMap<String, String>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(err) => return Err(AppError::Io(err)),
        };
        let body = match marker_of(text.trim()) {
            // A plaintext map from a build that predates sealing. Hand it back
            // and seal it in place; failing to re-seal must not fail the read,
            // or one bad boot costs the user every pairing they have.
            None => {
                let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&text) else {
                    // Unframed and not the legacy map: quarantine, like every
                    // other unreadable shape, so the user can re-pair.
                    return Ok(self.quarantine("the stored token map is not readable JSON"));
                };
                if let Err(err) = self.write_map(&map) {
                    log::warn!(target: "loca::keychain", "could not seal the existing token store: {err}");
                }
                return Ok(map);
            }
            Some(SEALED_MAGIC) => &text.trim()[SEALED_PREFIX.len()..],
            // Framed by a newer build. Quarantine rather than guess: the
            // Core-side store makes the same call for the same bytes.
            Some(other) => {
                return Ok(self.quarantine(&format!(
                    "it is framed as \"{other}\", which this build cannot read"
                )));
            }
        };
        let sealed = match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, body)
        {
            Ok(bytes) => bytes,
            Err(_) => return Ok(self.quarantine("the sealed token store is not valid base64")),
        };
        let plain = match self.boxed.unseal(&sealed) {
            Ok(plain) => plain,
            // Reachable with no attacker: a device-wide credential reset
            // destroys AndroidKeyStore keys while leaving this file behind.
            Err(reason) => return Ok(self.quarantine(&reason)),
        };
        match serde_json::from_slice(&plain) {
            Ok(map) => Ok(map),
            Err(_) => Ok(self.quarantine("the opened token store was not valid JSON")),
        }
    }

    /// Treat an unopenable store as empty and move it aside. Never an error: a
    /// phone that cannot start because a key rotated is worse than one asking to
    /// be paired again, and the bytes stay on disk as evidence.
    fn quarantine(&self, reason: &str) -> HashMap<String, String> {
        let mut aside = self.path.clone().into_os_string();
        aside.push(".unreadable");
        log::warn!(
            target: "loca::keychain",
            "the paired-core token store cannot be opened ({reason}); \
             moving it to {} and asking for a fresh pairing",
            PathBuf::from(aside.clone()).display(),
        );
        let _ = std::fs::rename(&self.path, PathBuf::from(aside));
        HashMap::new()
    }

    fn write_map(&self, map: &HashMap<String, String>) -> AppResult<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let plain = serde_json::to_vec(map)?;
        let sealed = self
            .boxed
            .seal(&plain)
            .map_err(|reason| AppError::External(format!("keychain:seal_failed:{reason}")))?;
        let framed = format!(
            "{SEALED_PREFIX}{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, sealed)
        );
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, framed.as_bytes())?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// The seal/open pair, narrowed to what this file needs so the host tests can
/// supply a double without pulling in the Core's error type on desktop.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
trait SealBytes: Send + Sync {
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, String>;
    fn unseal(&self, sealed: &[u8]) -> Result<Vec<u8>, String>;
}

#[cfg(target_os = "android")]
impl SealBytes for KeystoreBox {
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        loca_core_ffi::loca_core::secrets::android::SecretBox::seal(self, plaintext)
            .map(|v| v)
            .map_err(|err| err.message)
    }
    fn unseal(&self, sealed: &[u8]) -> Result<Vec<u8>, String> {
        loca_core_ffi::loca_core::secrets::android::SecretBox::unseal(self, sealed)
            .map_err(|err| err.message)
    }
}

impl<B: SealBytes> KeychainStore for SealedFileKeychain<B> {
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

/// Pick the keychain backing for this platform + channel. See the module
/// header for why the dev channel cannot use the real OS keychain.
fn store(handle: &AppHandle) -> AppResult<Box<dyn KeychainStore>> {
    #[cfg(target_os = "android")]
    {
        // UNDEFENDED BY TEST: a host test binary cannot compile this arm, let
        // alone construct a `KeystoreBox`. The sealing POLICY is covered on the
        // host (`sealed_keychain`); that this line passes it a real
        // `KeystoreBox` is covered by the device checklist.
        return Ok(sealed_keychain(
            keychain_file_path(handle)?,
            KeystoreBox::new(keychain_service()),
        ));
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

/// The sealing policy, separated from the platform choice so a host test can
/// drive it with a double and assert against the FILE rather than a round trip.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn sealed_keychain<B: SealBytes + 'static>(path: PathBuf, boxed: B) -> Box<dyn KeychainStore> {
    Box::new(SealedFileKeychain { path, boxed })
}

/// Map a raw `keyring_core::Error` into one of three generic codes: the raw
/// detail (file paths, COM HRESULTs, DBus method names) leaks host topology to
/// anyone who can read the client logs.
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

    // --- android sealing -------------------------------------------------
    //
    // These run on the HOST, so what is under test is the framing, the
    // migration off a plaintext file, and what happens when an envelope cannot
    // be opened. The JNI half is covered by the android build and the device
    // checklist.

    /// Not encryption and not pretending to be. Reversible so a test can assert
    /// the plaintext is gone from the FILE while the round trip still works.
    #[derive(Default)]
    struct FakeSeal {
        fail_unseal: std::sync::atomic::AtomicBool,
        fail_seal: std::sync::atomic::AtomicBool,
    }

    impl SealBytes for FakeSeal {
        fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
            if self.fail_seal.load(Ordering::Relaxed) {
                return Err("keystore refused".into());
            }
            Ok(plaintext.iter().map(|b| b ^ 0x5A).collect())
        }
        fn unseal(&self, sealed: &[u8]) -> Result<Vec<u8>, String> {
            if self.fail_unseal.load(Ordering::Relaxed) {
                return Err("no keystore key for the alias".into());
            }
            Ok(sealed.iter().map(|b| b ^ 0x5A).collect())
        }
    }

    fn sealed_store() -> SealedFileKeychain<FakeSeal> {
        SealedFileKeychain {
            path: unique_keychain_path(),
            boxed: FakeSeal::default(),
        }
    }

    fn aside_of(path: &std::path::Path) -> PathBuf {
        let mut p = path.to_path_buf().into_os_string();
        p.push(".unreadable");
        PathBuf::from(p)
    }

    #[test]
    fn sealed_keychain_round_trips_and_deletes() {
        let store = sealed_store();
        set_token(&store, "01H8XGJWBWBAQ4WG", "bearer-xyz").unwrap();
        assert_eq!(
            get_token(&store, "01H8XGJWBWBAQ4WG").unwrap(),
            Some("bearer-xyz".into())
        );
        delete_token(&store, "01H8XGJWBWBAQ4WG").unwrap();
        assert_eq!(get_token(&store, "01H8XGJWBWBAQ4WG").unwrap(), None);
    }

    /// A round-trip assertion alone passes against a store that writes
    /// plaintext, so the FILE is read back and checked for the token.
    #[test]
    fn what_lands_on_disk_is_sealed_rather_than_the_token() {
        let store = sealed_store();
        set_token(&store, "core-1", "bearer-xyz").unwrap();
        let raw = std::fs::read_to_string(&store.path).unwrap();
        assert!(raw.starts_with(SEALED_PREFIX), "not framed: {raw}");
        assert!(!raw.contains("bearer-xyz"), "the token survived: {raw}");
        assert!(!raw.contains("core:core-1"), "the account survived: {raw}");
    }

    #[test]
    fn a_plaintext_store_from_an_earlier_build_is_read_and_then_sealed_in_place() {
        let store = sealed_store();
        std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        std::fs::write(&store.path, r#"{"core:core-1":"legacy-token"}"#).unwrap();

        // Still readable, which is what stops an upgrade unpairing every core.
        assert_eq!(
            get_token(&store, "core-1").unwrap(),
            Some("legacy-token".into())
        );
        let raw = std::fs::read_to_string(&store.path).unwrap();
        assert!(raw.starts_with(SEALED_PREFIX), "not migrated: {raw}");
        assert!(!raw.contains("legacy-token"));
        assert_eq!(
            get_token(&store, "core-1").unwrap(),
            Some("legacy-token".into())
        );
    }

    #[test]
    fn a_plaintext_store_is_still_readable_when_sealing_it_fails() {
        let store = sealed_store();
        std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        std::fs::write(&store.path, r#"{"core:core-1":"legacy-token"}"#).unwrap();
        store.boxed.fail_seal.store(true, Ordering::Relaxed);
        assert_eq!(
            get_token(&store, "core-1").unwrap(),
            Some("legacy-token".into())
        );
    }

    #[test]
    fn a_store_the_keystore_can_no_longer_open_reads_as_empty_and_is_moved_aside() {
        let store = sealed_store();
        set_token(&store, "core-1", "bearer-xyz").unwrap();

        store.boxed.fail_unseal.store(true, Ordering::Relaxed);
        // Empty rather than an error: the user re-pairs, the app still starts.
        assert_eq!(get_token(&store, "core-1").unwrap(), None);
        assert!(!store.path.exists());
        assert!(
            aside_of(&store.path).exists(),
            "the evidence was destroyed rather than kept"
        );
    }

    /// Drives the POLICY rather than the platform arm: given a seal primitive,
    /// the store it builds must leave no plaintext on disk. That the android arm
    /// of `store()` passes a real `KeystoreBox` is not covered here (see the
    /// UNDEFENDED note at that call site).
    #[test]
    fn a_sealed_keychain_built_by_the_policy_writes_no_plaintext() {
        let path = unique_keychain_path();
        let store = sealed_keychain(path.clone(), FakeSeal::default());
        store.set("core:c1", "bearer-xyz").unwrap();
        assert_eq!(store.get("core:c1").unwrap(), Some("bearer-xyz".into()));
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with(SEALED_PREFIX), "not framed: {raw}");
        assert!(!raw.contains("bearer-xyz"), "the token survived: {raw}");
    }

    /// An unframed non-JSON file must not wedge both `get_token` and
    /// `set_token`: the user has to be able to re-pair their way out.
    #[test]
    fn an_unframed_non_json_file_is_quarantined_rather_than_bricking_the_keychain() {
        let store = sealed_store();
        std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        std::fs::write(&store.path, "this is not json and not framed").unwrap();

        assert_eq!(get_token(&store, "c1").unwrap(), None, "get must not error");
        // The decisive half: the user can still pair again afterwards.
        set_token(&store, "c1", "fresh").unwrap();
        assert_eq!(get_token(&store, "c1").unwrap(), Some("fresh".into()));
    }

    /// The two stores share one on-disk format, so a marker neither can read
    /// must be quarantined by both rather than destroyed or hard-errored on.
    #[test]
    fn a_store_framed_by_a_newer_build_is_quarantined_rather_than_read() {
        let store = sealed_store();
        std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        std::fs::write(&store.path, "LOCAK2:YWJjZA==").unwrap();

        assert_eq!(get_token(&store, "c1").unwrap(), None);
        assert_eq!(
            std::fs::read_to_string(aside_of(&store.path)).unwrap(),
            "LOCAK2:YWJjZA==",
            "the envelope must survive as evidence",
        );
    }

    /// The mirror-keeping table: the identical cases exist in
    /// `loca_core::secrets::android`'s suite, so changing one copy of
    /// `marker_of` without the other turns one of the two red.
    #[test]
    fn the_two_stores_classify_the_same_bytes_the_same_way() {
        let cases: [(&str, Option<&str>); 6] = [
            ("LOCAK1:YWJjZA==", Some("LOCAK1")),
            ("LOCAK2:YWJjZA==", Some("LOCAK2")),
            ("{\"core:a\":\"t\"}", None),
            ("YWJjZA==", None),
            ("http://x", None),
            (":abcd", None),
        ];
        for (input, want) in cases {
            assert_eq!(marker_of(input), want, "classifying {input:?}");
        }
    }

    /// This is a `grep`, and it is documented as one: exact strings must (or
    /// must not) appear in `SecretBox.kt` once comments are stripped. It does
    /// NOT verify the cryptography.
    ///
    /// - Covered: the file is gone (an `include_str!` compile error), or a named
    ///   declaration or one of these assignments is missing or changed.
    /// - NOT covered: any rearrangement that keeps the strings and changes what
    ///   runs. Only compiling and running the Kotlin can close that, which needs
    ///   a JVM test and a gradle step in CI.
    #[test]
    fn the_kotlin_secret_box_still_declares_the_strings_this_test_searches_for() {
        let code = kotlin_code_only(SECRET_BOX_KT);
        let (package, object) = SECRET_BOX_CLASS.rsplit_once('.').unwrap();
        for needle in [
            format!("fun {SECRET_BOX_SEAL}("),
            format!("fun {SECRET_BOX_UNSEAL}("),
            format!("package {package}"),
            format!("object {object}"),
            "@JvmStatic".to_string(),
            "= \"AndroidKeyStore\"".to_string(),
            "= \"AES/GCM/NoPadding\"".to_string(),
            "KeyStore.getInstance(PROVIDER)".to_string(),
            // Each of these is a security property the class's own doc asserts.
            ".setKeySize(256)".to_string(),
            ".setRandomizedEncryptionRequired(true)".to_string(),
            "KeyProperties.BLOCK_MODE_GCM".to_string(),
            "KeyProperties.ENCRYPTION_PADDING_NONE".to_string(),
            "GCMParameterSpec(TAG_BITS".to_string(),
            "KeyGenerator.getInstance(".to_string(),
            // A hardcoded key needs one of these two, and neither belongs in a
            // KeyStore-backed class. Cheap, and it closes the specific shape
            // F-G demonstrated even though it cannot close the general one.
            "SecretKeySpec".to_string(),
            "ByteArray(32)".to_string(),
        ] {
            let banned = needle == "SecretKeySpec" || needle == "ByteArray(32)";
            if banned {
                assert!(
                    !code.contains(&needle),
                    "SecretBox.kt now contains {needle:?}, which is how a key stops \
                     coming from the AndroidKeyStore and starts being a constant \
                     compiled into the APK",
                );
            } else {
                assert!(
                    code.contains(&needle),
                    "SecretBox.kt no longer declares {needle:?}",
                );
            }
        }
    }

    /// Kotlin with `//` and `/* */` removed, so the tripwire cannot be satisfied
    /// by a doc comment describing code that is gone. Deliberately naive: it does
    /// not understand string literals, which is safe only because no string in
    /// `SecretBox.kt` contains a comment opener.
    fn kotlin_code_only(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut rest = src;
        while let Some(start) = rest.find("/*") {
            out.push_str(&rest[..start]);
            match rest[start..].find("*/") {
                Some(end) => rest = &rest[start + end + 2..],
                None => {
                    rest = "";
                    break;
                }
            }
        }
        out.push_str(rest);
        out.lines()
            .map(|line| match line.find("//") {
                Some(at) => &line[..at],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The stripper is itself an instrument, so it gets its own check: one that
    /// returned its input unchanged would let the tripwire pass on prose.
    #[test]
    fn a_comment_cannot_satisfy_the_secret_box_tripwire() {
        let gutted = "/** uses AndroidKeyStore and fun unseal( */\nobject X { }\n";
        let code = kotlin_code_only(gutted);
        assert!(!code.contains("AndroidKeyStore"), "block comment survived");
        assert!(!code.contains("fun unseal("), "block comment survived");
        assert!(code.contains("object X"), "code was stripped too");

        let line = "val a = 1 // AndroidKeyStore\nval b = 2\n";
        let code = kotlin_code_only(line);
        assert!(!code.contains("AndroidKeyStore"), "line comment survived");
        assert!(code.contains("val a = 1") && code.contains("val b = 2"));
    }
}
