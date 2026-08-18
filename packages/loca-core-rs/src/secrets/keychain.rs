//! The OS keychain half of the secure store: macOS Keychain, the iOS
//! data-protection keychain, Windows Credential Manager, and libsecret through
//! secret-service on the BSDs and Linux, all via keyring-core.
//!
//! Android has no backend wired here, so [`select`](super::store::select) never
//! builds a [`KeychainStore`] there and the dotfile is the only store. Closing
//! that (an Android Keystore backend) is M4 and lands as one more
//! [`SecureStore`] implementation.
//!
//! Raw backend errors stay out of `core.log`. Their text can carry host topology
//! (file paths, COM HRESULTs, dbus interface and method names), so the classified
//! reason goes to WARN and the raw text to DEBUG, which is stderr-only because
//! the file sink starts at INFO.

use std::sync::OnceLock;

use keyring_core::{Entry, Error as KeyringError};

use super::store::SecureStore;
use crate::config::Channel;
use crate::error::{internal, Result};

/// Per-channel keychain service. Stable keeps the bare
/// `com.rzkyif.loca.core` so an existing install's sealed master key keeps
/// resolving; dev and latest are suffixed. The suffix comes from
/// [`Channel::suffix`] and is not re-derived here.
///
/// This string is a byte-exact parity surface (D-012). Change it and the ported
/// core finds no key, generates a fresh one, re-encrypts the vault over the old
/// one, and loses the sealed TLS key with it, which breaks every paired client's
/// pin with nothing in the logs pointing at the keychain.
pub fn service_name(channel: Channel) -> String {
    format!("com.rzkyif.loca.core{}", channel.suffix())
}

pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, key: &str) -> Result<Entry> {
        if !ensure_default_store() {
            return Err(internal("no OS keychain is available on this platform"));
        }
        Entry::new(&self.service, key).map_err(|err| classify("open", err))
    }
}

// No derived Debug: the type is reachable from the vault, and a derive here is
// how a service name and, one refactor later, a value end up in someone's
// `{:?}`.
impl std::fmt::Debug for KeychainStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeychainStore")
    }
}

impl SecureStore for KeychainStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        match self.entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(err) => Err(classify("get", err)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.entry(key)?
            .set_password(value)
            .map_err(|err| classify("set", err))
    }

    fn delete(&self, key: &str) -> Result<()> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(err) => Err(classify("delete", err)),
        }
    }
}

/// Register the platform credential store as keyring-core's process-global
/// default, once, and report whether one is in place.
///
/// Two guards, and the second is the load-bearing one. `set_default_store`
/// writes a process-global, and in the Stage 4 mobile build this library is
/// linked into the Tauri client, which registers its own store for the paired
/// core tokens. Replacing it would strand the client's tokens, so an existing
/// registration is left alone. A mobile frontend that wants a specific store
/// passes it to `Vault::with_store` instead, and then nothing here ever runs.
fn ensure_default_store() -> bool {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    *REGISTERED.get_or_init(|| {
        if keyring_core::get_default_store().is_some() {
            tracing::debug!(
                target: "loca::secure-store",
                "a default credential store is already registered; using it as-is",
            );
            return true;
        }
        register_platform_store()
    })
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd"
))]
fn register_platform_store() -> bool {
    // macOS takes the file-based keychain; iOS has only the data-protection
    // keychain, and the crate's `keychain` module is macOS-only.
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new();
    #[cfg(target_os = "ios")]
    let store = apple_native_keyring_store::protected::Store::new();
    #[cfg(target_os = "windows")]
    let store = windows_native_keyring_store::Store::new();
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
    let store = dbus_secret_service_keyring_store::Store::new();

    match store {
        Ok(store) => {
            keyring_core::set_default_store(store);
            true
        }
        Err(err) => {
            tracing::debug!(target: "loca::secure-store", "keychain store init failed: {err}");
            tracing::warn!(
                target: "loca::secure-store",
                "the OS keychain is unavailable ({}); secrets fall back to a file",
                reason(&err),
            );
            false
        }
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd"
)))]
fn register_platform_store() -> bool {
    false
}

fn classify(op: &str, err: KeyringError) -> crate::error::AppError {
    // `{err}`, never `{err:?}`. Two variants carry the raw credential blob:
    // `BadEncoding(Vec<u8>)` (the stored bytes were not UTF-8) and
    // `BadDataFormat(Vec<u8>, _)` (a store that encrypts or otherwise transforms
    // secrets handed back something it could not undo, which makes it the
    // likelier of the two on Windows and iOS). Both drop the blob in `Display`
    // and print it in the derived `Debug`, and `scrub_secrets` cannot save that
    // case: a byte-vector Debug rendering matches none of its patterns. See
    // D-017.
    tracing::debug!(target: "loca::secure-store", "keychain {op} failed: {err}");
    internal(format!("keychain {op} failed: {}", reason(&err)))
}

/// A fixed vocabulary, so nothing platform-specific reaches a persisted line.
/// Every variant that carries a credential blob is listed explicitly rather than
/// swept into the catch-all, so the hazard stays visible to the next reader.
fn reason(err: &KeyringError) -> &'static str {
    match err {
        KeyringError::NoEntry => "no entry",
        KeyringError::Ambiguous(_) => "ambiguous entry",
        KeyringError::BadEncoding(_) => "bad encoding",
        KeyringError::BadDataFormat(_, _) => "malformed stored data",
        KeyringError::TooLong(_, _) => "value too long",
        KeyringError::Invalid(_, _) => "invalid argument",
        KeyringError::NoStorageAccess(_) => "no storage access",
        KeyringError::NotSupportedByStore(_) => "unsupported by this store",
        _ => "platform failure",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // Pinned, not asserted loosely: an existing install's entry lives under
    // exactly these strings. See D-012.
    #[test]
    fn the_service_name_is_channel_namespaced_and_stable_stays_bare() {
        assert_eq!(service_name(Channel::Stable), "com.rzkyif.loca.core");
        assert_eq!(service_name(Channel::Dev), "com.rzkyif.loca.core-dev");
        assert_eq!(service_name(Channel::Latest), "com.rzkyif.loca.core-latest");
    }

    #[test]
    fn debug_reveals_nothing() {
        assert_eq!(
            format!("{:?}", KeychainStore::new("com.rzkyif.loca.core")),
            "KeychainStore"
        );
    }
}
