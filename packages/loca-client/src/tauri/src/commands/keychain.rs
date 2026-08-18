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
//   - android (all channels): `SealedFileKeychain`, the same file at the same
//     path with its contents sealed under an AES-256-GCM key held in the
//     AndroidKeyStore (`SecretBox.kt`, called over JNI below). Before this the
//     token map was plaintext JSON leaning on the app sandbox plus
//     `allowBackup="false"`; a plaintext file read is now a plaintext file that
//     decodes to nothing useful.
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

// --- android: sealed under an AndroidKeyStore key --------------------------

/// The Kotlin half, pulled in so that **deleting it fails the build**.
///
/// `tauri android init` regenerates the tree around `SecretBox.kt`, and a
/// silently-missing SecretBox would drop this store and the embedded Core's
/// vault master key back to plaintext, on a phone, with nothing red anywhere.
/// The file is a JNI callee, so nothing in Rust would otherwise reference it and
/// nothing in the gate would notice it going.
///
/// `include_str!` is a compile-time existence check on every android build, and
/// `the_kotlin_secret_box_still_declares_what_the_jni_layer_calls` below is a
/// content check that runs on the HOST, so it is in `deno task test` on every
/// platform rather than only where the APK is built. Neither is decoration:
/// both were watched failing against a removed file and a gutted one.
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
        /// **The class is resolved through the Activity's classloader, not with
        /// `FindClass`.** A JNI `FindClass` issued from a thread Rust created
        /// searches the SYSTEM classloader, which cannot see application
        /// classes, so it would fail with `ClassNotFoundException` for every
        /// call made off the UI thread, which is all of them. This is the
        /// single most common Android JNI mistake and it is invisible until a
        /// device runs it.
        ///
        /// Nothing is cached between calls. Sealing happens a handful of times
        /// per boot, and a cached class reference would be process-global state
        /// of exactly the kind D-010 keeps out of this codebase.
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

            // A Java exception is still pending on the env after a failed call
            // and would poison the NEXT JNI call on this thread with a
            // misleading error. Clear it here, and keep the description out of
            // the message: an exception raised inside the cipher is exactly
            // where a buffer would leak into a log (D-017's rule, applied to
            // the JVM's error channel rather than keyring-core's).
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

/// The token map, sealed. Same path and same JSON shape as [`FileKeychain`], so
/// the seam is the bytes on disk and nothing above this changes.
///
/// The seal/open primitive is injected rather than constructed here so the
/// migration and the failure handling are driven by a host test; the real one is
/// JNI and is unreachable from a host test binary (D-117).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
struct SealedFileKeychain<B> {
    path: PathBuf,
    boxed: B,
}

/// What a sealed keychain file looks like. The framing is the Core's, reused
/// deliberately: one on-disk format for both of this device's secret stores
/// rather than two that can disagree about a version marker.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const SEALED_MAGIC: &str = "LOCAK1";
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const SEALED_PREFIX: &str = "LOCAK1:";

/// The version marker of a framed value, or `None` when it carries none.
///
/// **A mirror of `loca_core::secrets::android::marker_of`**, byte for byte. It
/// cannot be shared: `loca-core-ffi` is a dependency only under
/// `cfg(any(android, ios))`, so a host test binary cannot reach it, and this
/// file's tests all run on the host.
///
/// The duplication is kept honest by
/// `the_two_stores_classify_the_same_bytes_the_same_way`, which exists in BOTH
/// suites over the SAME input table. Changing one copy without the other turns
/// one of them red. That is the concrete answer to F-F: the two stores share a
/// format, so they must share a classification.
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
            // A plaintext map written by a build that predates sealing. Hand it
            // back and seal it in place; failing to re-seal must not fail the
            // read, or one bad boot costs the user every pairing they have.
            None => {
                let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&text) else {
                    // **The fourth corner, and it used to BRICK.** An unframed
                    // file that is not the legacy JSON map propagated a raw
                    // serde error out of BOTH `get_token` and `set_token`, so
                    // the user could not even re-pair their way out: every
                    // keychain operation on that device failed forever and the
                    // file was never moved aside. Its three sibling corners
                    // (bad base64, unseal failure, opened-but-not-JSON) all
                    // quarantine. This one now does too.
                    return Ok(self.quarantine("the stored token map is not readable JSON"));
                };
                if let Err(err) = self.write_map(&map) {
                    log::warn!(target: "loca::keychain", "could not seal the existing token store: {err}");
                }
                return Ok(map);
            }
            Some(SEALED_MAGIC) => &text.trim()[SEALED_PREFIX.len()..],
            // Framed by a build newer than this one. Quarantine rather than
            // guess: the Core-side store makes the same call for the same bytes
            // (`secrets/android.rs`), and the two disagreeing about one shared
            // format is exactly what F-F found.
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

    /// Treat an unopenable store as empty and move it aside. The user re-pairs,
    /// which is a prompt rather than a broken app; the bytes stay on disk so a
    /// support case has something to look at. Never an error: a phone that
    /// cannot start because a key rotated is strictly worse than one that asks
    /// to be paired again.
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

/// Pick the keychain backing for this platform + channel. Android has no
/// keyring-core OS backend wired, so it seals the same app-private file under an
/// AndroidKeyStore key. On desktop, the unsigned `dev` build can't use the macOS
/// keychain (writes report success but never persist), so it stores tokens in a
/// file; the signed stable/latest bundles use the real OS keychain.
fn store(handle: &AppHandle) -> AppResult<Box<dyn KeychainStore>> {
    #[cfg(target_os = "android")]
    {
        // UNDEFENDED BY TEST, and labelled rather than guarded.
        //
        // A host test binary cannot construct a `KeystoreBox` (it is JNI) and
        // cannot compile this arm at all, so no behavioural test in this crate
        // can observe it. The previous attempt was a source-reading guard, and
        // F-E got past it with a TODO comment containing the word it searched
        // for. A guard that reports success on a broken tree is worse than a
        // stated gap, so this is a stated gap.
        //
        // What IS defended: the policy below (`sealed_keychain`, given a seal
        // primitive, produces a store whose FILE carries no plaintext) is
        // driven behaviourally on the host. What is not: that this one line
        // hands it a real `KeystoreBox`. The device checklist covers it.
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

/// The sealing policy, separated from the platform choice so it can be driven on
/// a host with a double.
///
/// This is the half worth testing: given a seal primitive, the resulting store
/// must actually seal. `a_sealed_keychain_built_by_the_policy_writes_no_plaintext`
/// asserts it against the FILE rather than a round trip.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn sealed_keychain<B: SealBytes + 'static>(path: PathBuf, boxed: B) -> Box<dyn KeychainStore> {
    Box::new(SealedFileKeychain { path, boxed })
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
    // These run on the HOST. The real seal is JNI and is unreachable from a
    // host test binary at all, so what is under test here is the framing, the
    // migration off a plaintext file, and what happens when an envelope cannot
    // be opened. Those are where the ordering and the failure modes live; the
    // JNI half is covered by `deno task check:android` compiling it and by the
    // device checklist in the M4 status.

    /// Not encryption and not pretending to be. Reversible so the tests can
    /// assert the plaintext is gone from the FILE while the round trip still
    /// works, which is the pair that catches a store that only pretends to seal.
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

    /// The property the whole workstream exists for. A round-trip assertion
    /// alone passes against a store that writes plaintext, so the FILE is read
    /// back and checked for the token.
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
        let aside = {
            let mut p = store.path.clone().into_os_string();
            p.push(".unreadable");
            PathBuf::from(p)
        };
        assert!(
            aside.exists(),
            "the evidence was destroyed rather than kept"
        );
    }

    /// The behavioural replacement for the source-reading guard that F-E got
    /// past with a TODO comment. It drives the POLICY rather than reading the
    /// platform arm: given a seal primitive, the store the policy builds must
    /// leave no plaintext on disk.
    ///
    /// What it does not cover, stated rather than implied: that the android arm
    /// of `store()` calls this with a real `KeystoreBox`. That arm cannot be
    /// compiled or run by a host test binary, so it is labelled UNDEFENDED at
    /// the call site and covered by the device checklist instead.
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

    /// F-F: this input used to fail `get_token` AND `set_token` forever with a
    /// raw serde message, so the user could not re-pair their way out and the
    /// only recovery was clearing app data. Its three sibling corners all
    /// quarantined; this one now does too.
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

    /// F-F's other half: the two stores share one on-disk format and used to
    /// disagree about the same bytes. The Core-side store accepted `LOCAK2:` as
    /// a secret and destroyed it; this one hard-errored forever.
    #[test]
    fn a_store_framed_by_a_newer_build_is_quarantined_rather_than_read() {
        let store = sealed_store();
        std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        std::fs::write(&store.path, "LOCAK2:YWJjZA==").unwrap();

        assert_eq!(get_token(&store, "c1").unwrap(), None);
        let aside = {
            let mut p = store.path.clone().into_os_string();
            p.push(".unreadable");
            PathBuf::from(p)
        };
        assert_eq!(
            std::fs::read_to_string(aside).unwrap(),
            "LOCAK2:YWJjZA==",
            "the envelope must survive as evidence",
        );
    }

    /// The mirror-keeping table. `marker_of` here is a byte-for-byte copy of
    /// `loca_core::secrets::android::marker_of`, which this crate cannot import
    /// off mobile. **The identical table exists in that module's suite**, so
    /// changing one copy without the other turns one of the two red. That is the
    /// answer to "one on-disk format, two implementations that disagreed".
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

    /// **This is a `grep`, and it is documented as one.**
    ///
    /// It asserts that a list of exact strings appears in `SecretBox.kt` after
    /// comments are stripped. It does NOT verify the cryptography, and an
    /// earlier version of this comment claimed it pinned "the pair that decides
    /// whether the key is in hardware or in the process". F-G showed that claim
    /// false by replacing the key with a hardcoded `SecretKeySpec(ByteArray(32))`
    /// and leaving `KeyStore.getInstance(PROVIDER)` in an unreferenced function:
    /// every string below was still present and the gate was green while the
    /// sealing key was a constant compiled into the APK.
    ///
    /// So, stating the query rather than the property it wants:
    ///
    /// - **Covered:** the file is gone (an `include_str!` compile error), or a
    ///   named declaration or one of these assignments is missing or changed.
    ///   `setKeySize(128)` and `setRandomizedEncryptionRequired(false)` both
    ///   survived before and are pinned now.
    /// - **NOT covered:** any rearrangement that keeps the strings and changes
    ///   what runs. A Kotlin identifier appearing in a file says nothing about
    ///   which function the cipher is initialised from, and no source search can
    ///   close that. Only compiling and running the Kotlin can, which needs a
    ///   JVM test under `gen/android/app/src/test/` and a gradle step in CI.
    ///   Filed on the interfaces queue; until it lands this hole is open and
    ///   named rather than papered over.
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
            // Added after F-G: each of these survived a mutation before, and
            // each is a security property the class's own doc asserts.
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

    /// Kotlin with `//` and `/* */` removed, so a tripwire cannot be satisfied
    /// by a doc comment that still describes code which is gone.
    ///
    /// Deliberately naive: it does not understand string literals, which is
    /// safe here only because no string in `SecretBox.kt` contains a comment
    /// opener. `a_comment_cannot_satisfy_the_secret_box_tripwire` is what keeps
    /// that honest, by proving the stripper removes what it claims to.
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

    /// The stripper is itself an instrument, so it gets its own check on a
    /// known-bad and a known-good input (D-146). Without this, a stripper that
    /// returned its input unchanged would restore the exact hole above and the
    /// tripwire would go back to passing on prose.
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
