//! Where the vault's master key is kept, and the policy that picks.
//!
//! One opaque entry, addressed by key name. The OS keychain where one is
//! usable, a chmod-600 dotfile under the channel root otherwise. The layered
//! policy in [`LayeredStore`] is the part worth reading: a keychain write is
//! read back and compared before it is believed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::Channel;
use crate::error::{internal, Result};
use crate::files;

/// Durable storage for one opaque value. Blocking on purpose: a keychain call
/// is platform FFI that can prompt on macOS and can wait on a dbus round trip,
/// so the vault runs every implementation inside `spawn_blocking` rather than
/// letting one stall a tokio worker.
///
/// `Debug` is required and every implementation writes it by hand, printing the
/// type name and nothing else. A derive would put the service name, the root
/// path, and one refactor later a stored value, into any `{:?}` that reaches a
/// log line or a panic message.
pub trait SecureStore: Send + Sync + std::fmt::Debug {
    /// `Ok(None)` for "no such entry". An empty stored value counts as absent.
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    /// Idempotent: removing an entry that is not there is success.
    fn delete(&self, key: &str) -> Result<()>;
}

/// The dotfile holding one key, a hidden file directly under the channel root.
/// The crate reaches it through [`super::master_key_file`], which is the only
/// key there is.
pub(crate) fn key_file(root: &Path, key: &str) -> PathBuf {
    root.join(format!(".{key}"))
}

/// The transient target of a [`key_file`] write. `files::write_secret_atomic`
/// uses a fixed `<path>.tmp` precisely so a deny list can name it, which only
/// works if both sides derive it the same way.
pub(crate) fn key_tmp_file(root: &Path, key: &str) -> PathBuf {
    let mut path = key_file(root, key).into_os_string();
    path.push(".tmp");
    PathBuf::from(path)
}

/// The chmod-600 dotfile fallback. Also the only store on the dev channel and
/// on Android.
pub struct FileStore {
    root: PathBuf,
}

impl FileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn path(&self, key: &str) -> PathBuf {
        key_file(&self.root, key)
    }
}

impl std::fmt::Debug for FileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileStore")
    }
}

impl SecureStore for FileStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        let path = self.path(key);
        match std::fs::read_to_string(&path) {
            // Trimmed, so a hand-restored file with a trailing newline still
            // loads, and empty means absent rather than a zero-length key.
            Ok(text) => Ok(Some(text.trim().to_string()).filter(|t| !t.is_empty())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(internal(format!(
                "failed to read {}: {err}",
                path.display()
            ))),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        // Atomic and 0600-at-creation. Atomic because an interrupted write
        // truncates the master key, and a truncated master key is a vault lost
        // for good; the mode because the file is the key in plaintext.
        files::write_secret_atomic_blocking(&self.path(key), value.as_bytes())
    }

    fn delete(&self, key: &str) -> Result<()> {
        match std::fs::remove_file(self.path(key)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

/// The keychain, with the dotfile behind it.
///
/// Reads prefer the keychain and fall through. Writes are sealed in the
/// keychain only if a read-back returns exactly what was written, and land in
/// the dotfile otherwise. Deletes hit both.
pub struct LayeredStore {
    keychain: Arc<dyn SecureStore>,
    file: FileStore,
}

impl LayeredStore {
    pub fn new(keychain: Arc<dyn SecureStore>, file: FileStore) -> Self {
        Self { keychain, file }
    }

    /// Write, read back, compare. Not defensive coding: a keychain can accept a
    /// write it will never serve back, and every instance of that is silent.
    /// An unsigned or rebuilt macOS binary gets items scoped to a code-signing
    /// identity that has changed by the next launch; a headless Linux box can
    /// take the write into a locked or ephemeral collection; a mismatched iOS
    /// access group writes where we cannot read.
    ///
    /// Believing the bare success means no dotfile is written and this process
    /// keeps using its in-memory key until it exits. The failure then surfaces a
    /// boot later as a missing key, the vault is re-encrypted under a fresh one,
    /// and the sealed TLS key goes with it, so every paired client's pin stops
    /// matching. One extra read, once per install, converts that into an
    /// immediate logged fallback.
    fn seal_verified(&self, key: &str, value: &str) -> bool {
        if let Err(err) = self.keychain.set(key, value) {
            tracing::warn!(
                target: "loca::secure-store",
                "OS keychain refused \"{key}\": {}", err.message,
            );
            return false;
        }
        match self.keychain.get(key) {
            Ok(Some(read_back)) if read_back == value => true,
            Ok(found) => {
                tracing::warn!(
                    target: "loca::secure-store",
                    "OS keychain accepted \"{key}\" but read-back {}; \
                     treating the keychain as unavailable and using the file fallback",
                    if found.is_none() { "found no entry" } else { "returned a different value" },
                );
                false
            }
            Err(err) => {
                tracing::warn!(
                    target: "loca::secure-store",
                    "OS keychain accepted \"{key}\" but read-back failed ({}); \
                     treating the keychain as unavailable and using the file fallback",
                    err.message,
                );
                false
            }
        }
    }
}

impl std::fmt::Debug for LayeredStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LayeredStore")
    }
}

impl SecureStore for LayeredStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        match self.keychain.get(key) {
            Ok(Some(value)) if !value.is_empty() => return Ok(Some(value)),
            Ok(_) => {}
            // A keychain that cannot be reached is not an error: it is the
            // condition the dotfile exists for.
            Err(err) => tracing::warn!(
                target: "loca::secure-store",
                "OS keychain read of \"{key}\" failed ({}); trying the file fallback",
                err.message,
            ),
        }
        self.file.get(key)
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        if self.seal_verified(key, value) {
            tracing::info!(target: "loca::secure-store", "sealed \"{key}\" in the OS keychain");
            return Ok(());
        }
        self.file.set(key, value)?;
        tracing::warn!(
            target: "loca::secure-store",
            "stored \"{key}\" in {}: the OS keychain is unavailable. Back up this file \
             or stored secrets are lost on reinstall.",
            self.file.path(key).display(),
        );
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        // Both copies, and a keychain failure must not stop the file delete:
        // uninstall has to leave nothing behind.
        if let Err(err) = self.keychain.delete(key) {
            tracing::warn!(
                target: "loca::secure-store",
                "OS keychain delete of \"{key}\" failed: {}", err.message,
            );
        }
        self.file.delete(key)
    }
}

/// Pick the backing store for this platform and channel. The one place a
/// backend is chosen.
///
/// The dev channel never touches the OS keychain, on any platform. A dev build
/// is unsigned and rebuilt at will, macOS scopes keychain items to a code
/// signature, so a key sealed by this build reads back empty after the next
/// build and the vault, sealed TLS key included, is silently orphaned.
///
/// Android has no keyring-core backend wired, and it does not get one here.
/// The KeyStore is reachable only through the JVM, so the sealed store is built
/// by the FRONTEND (which owns the JNI) and handed in through
/// `boot::boot_with_store`, which is the seam D-013 asked for. Reaching this
/// android branch at all therefore means no frontend supplied a store, and the
/// master key falls back to the app-private directory in plaintext, which is
/// why the line below is a `warn` rather than the `info` it used to be: per
/// D-125 the mitigation for a degraded secret store is loudness, not silence.
/// See [`super::android::SealedStore`].
///
/// Reads `LOCA_VAULT_FILE_STORE` the way the rest of the core reads a boolean
/// env var: set and truthy, rather than merely defined, so `=0` means off.
#[cfg(not(target_os = "android"))]
fn file_store_forced() -> bool {
    match std::env::var("LOCA_VAULT_FILE_STORE") {
        Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        Err(_) => false,
    }
}

pub fn select(root: &Path, channel: Channel) -> Arc<dyn SecureStore> {
    #[cfg(target_os = "android")]
    let forced = false;
    #[cfg(not(target_os = "android"))]
    let forced = file_store_forced();
    select_with(root, channel, forced)
}

/// [`select`] with the env read hoisted out.
///
/// The flag is injected rather than read inside because `std::env::set_var` is
/// unsound in a threaded test binary, so a test that set it would be asserting
/// on whatever the rest of the suite happened to leave behind. Same seam the
/// worker spawner uses for `$env` expansion.
pub fn select_with(root: &Path, channel: Channel, force_file_store: bool) -> Arc<dyn SecureStore> {
    #[cfg(target_os = "android")]
    {
        let _ = (channel, force_file_store);
        tracing::warn!(
            target: "loca::secure-store",
            "no frontend-supplied keystore on android: the vault master key rests in the \
             app-private data directory in PLAINTEXT. The mobile frontend is expected to \
             pass a secrets::android::SealedStore to boot_with_store.",
        );
        return Arc::new(FileStore::new(root));
    }
    #[cfg(not(target_os = "android"))]
    {
        if channel == Channel::Dev {
            tracing::info!(
                target: "loca::secure-store",
                "dev channel: keeping the vault master key in a file, not the OS keychain",
            );
            return Arc::new(FileStore::new(root));
        }
        // An explicit opt-out, for a core under test on a channel other than
        // `dev`. Both cores read it, and that is the point: without it the two
        // diverge off `dev` in a way that is invisible from outside. The Rust
        // core seals in the real OS keychain and can HANG on a boot that asks a
        // human about an item another binary owns; the Deno core's keychain
        // helper is a stub in a seeded home, so it silently uses the file store
        // and never hangs. A conformance run comparing them was comparing two
        // different configurations.
        //
        // It introduces no new storage mode: `dev` above already takes this path
        // unconditionally and Android ships on it today. What it changes is
        // reachability, so it is deliberately LOUD - `warn`, not `info`, because
        // a production core that has this set should say so on every boot rather
        // than downgrade quietly.
        if force_file_store {
            tracing::warn!(
                target: "loca::secure-store",
                "LOCA_VAULT_FILE_STORE is set: keeping the vault master key in a \
                 file rather than the OS keychain. This is for testing; unset it \
                 for a normal install.",
            );
            return Arc::new(FileStore::new(root));
        }
        Arc::new(LayeredStore::new(
            Arc::new(super::keychain::KeychainStore::new(
                super::keychain::service_name(channel),
            )),
            FileStore::new(root),
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::secrets::testing::{DeadStore, LyingStore, MemoryStore, WriteOnlyStore};

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn layered(root: &Path, keychain: Arc<dyn SecureStore>) -> LayeredStore {
        LayeredStore::new(keychain, FileStore::new(root))
    }

    #[test]
    fn the_dotfile_is_a_hidden_file_under_the_root_and_its_temp_is_nameable() {
        let root = Path::new("/x/.loca/stable/core");
        assert_eq!(
            key_file(root, "master-key"),
            Path::new("/x/.loca/stable/core/.master-key")
        );
        assert_eq!(
            key_tmp_file(root, "master-key"),
            Path::new("/x/.loca/stable/core/.master-key.tmp")
        );
    }

    #[test]
    fn file_store_round_trips_and_deletes_idempotently() {
        let dir = temp();
        let store = FileStore::new(dir.path());
        assert_eq!(store.get("master-key").unwrap(), None);
        store.set("master-key", "AAAA").unwrap();
        assert_eq!(store.get("master-key").unwrap().as_deref(), Some("AAAA"));
        store.delete("master-key").unwrap();
        store.delete("master-key").unwrap();
        assert_eq!(store.get("master-key").unwrap(), None);
    }

    #[test]
    fn file_store_writes_the_value_verbatim_with_no_trailing_newline() {
        let dir = temp();
        let store = FileStore::new(dir.path());
        store.set("master-key", "QUJD").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".master-key")).unwrap(),
            "QUJD"
        );
    }

    #[test]
    fn file_store_trims_a_hand_restored_file_and_treats_empty_as_absent() {
        let dir = temp();
        std::fs::write(dir.path().join(".master-key"), "QUJD\n").unwrap();
        assert_eq!(
            FileStore::new(dir.path())
                .get("master-key")
                .unwrap()
                .as_deref(),
            Some("QUJD")
        );
        std::fs::write(dir.path().join(".blank"), "  \n").unwrap();
        assert_eq!(FileStore::new(dir.path()).get("blank").unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn file_store_writes_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp();
        FileStore::new(dir.path())
            .set("master-key", "QUJD")
            .unwrap();
        let mode = std::fs::metadata(dir.path().join(".master-key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_working_keychain_wins_and_leaves_no_dotfile() {
        let dir = temp();
        let store = layered(dir.path(), Arc::new(MemoryStore::new()));
        store.set("master-key", "QUJD").unwrap();
        assert!(!dir.path().join(".master-key").exists());
        assert_eq!(store.get("master-key").unwrap().as_deref(), Some("QUJD"));
    }

    // The macOS unsigned-binary mode, and the reason the read-back exists.
    #[test]
    fn a_keychain_that_never_serves_the_write_back_falls_through_to_the_file() {
        let dir = temp();
        let store = layered(dir.path(), Arc::new(WriteOnlyStore::new()));
        store.set("master-key", "QUJD").unwrap();
        assert!(dir.path().join(".master-key").exists());
        assert_eq!(store.get("master-key").unwrap().as_deref(), Some("QUJD"));
    }

    #[test]
    fn a_keychain_that_returns_a_different_value_falls_through_to_the_file() {
        let dir = temp();
        let store = layered(dir.path(), Arc::new(LyingStore::new()));
        store.set("master-key", "QUJD").unwrap();
        // The real key is in the dotfile. Reads still prefer the keychain, so
        // this store keeps handing back its own answer for as long as it is
        // reachable; what the verification bought is that the key survives at
        // all, for the boot where the keychain has stopped answering.
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".master-key")).unwrap(),
            "QUJD"
        );
    }

    #[test]
    fn a_keychain_that_errors_on_every_call_falls_through_to_the_file() {
        let dir = temp();
        let store = layered(dir.path(), Arc::new(DeadStore));
        store.set("master-key", "QUJD").unwrap();
        assert_eq!(store.get("master-key").unwrap().as_deref(), Some("QUJD"));
        // And a delete still clears the file even though the keychain errors.
        store.delete("master-key").unwrap();
        assert_eq!(store.get("master-key").unwrap(), None);
    }

    #[test]
    fn an_empty_keychain_value_is_absent_so_the_file_wins() {
        let dir = temp();
        let keychain = Arc::new(MemoryStore::new());
        keychain.set("master-key", "").unwrap();
        FileStore::new(dir.path())
            .set("master-key", "QUJD")
            .unwrap();
        assert_eq!(
            layered(dir.path(), keychain)
                .get("master-key")
                .unwrap()
                .as_deref(),
            Some("QUJD")
        );
    }

    #[test]
    fn a_sealed_keychain_wins_over_a_stale_dotfile_without_touching_either() {
        let dir = temp();
        let keychain = Arc::new(MemoryStore::new());
        keychain.set("master-key", "S0VZ").unwrap();
        FileStore::new(dir.path())
            .set("master-key", "T0xE")
            .unwrap();
        let store = layered(dir.path(), keychain);
        assert_eq!(store.get("master-key").unwrap().as_deref(), Some("S0VZ"));
        // Non-mutating: no migration, no cleanup, both copies still there.
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".master-key")).unwrap(),
            "T0xE"
        );
    }

    #[test]
    fn delete_clears_both_copies() {
        let dir = temp();
        let keychain = Arc::new(MemoryStore::new());
        let store = layered(dir.path(), keychain.clone());
        keychain.set("master-key", "S0VZ").unwrap();
        FileStore::new(dir.path())
            .set("master-key", "T0xE")
            .unwrap();
        store.delete("master-key").unwrap();
        store.delete("master-key").unwrap();
        assert_eq!(keychain.get("master-key").unwrap(), None);
        assert!(!dir.path().join(".master-key").exists());
    }

    #[test]
    fn the_dev_channel_never_reaches_the_keychain() {
        let dir = temp();
        let store = select(dir.path(), Channel::Dev);
        store.set("master-key", "QUJD").unwrap();
        // A LayeredStore would have sealed this in the keychain and written no
        // file; the dotfile's existence is the proof the keychain was skipped.
        assert!(dir.path().join(".master-key").exists());
        assert_eq!(format!("{store:?}"), "FileStore");
    }

    #[test]
    fn stable_and_latest_get_the_layered_store() {
        let dir = temp();
        assert_eq!(
            format!("{:?}", select(dir.path(), Channel::Stable)),
            "LayeredStore"
        );
        assert_eq!(
            format!("{:?}", select(dir.path(), Channel::Latest)),
            "LayeredStore"
        );
    }

    /// The two cores must agree on where the vault lives, or a differential run
    /// compares two different configurations (D-125).
    ///
    /// Injected rather than env-driven: `std::env::set_var` is unsound in a
    /// threaded test binary, so a test that set it would assert on whatever the
    /// rest of the suite left behind.
    ///
    /// Mutation: ignore `force_file_store` in `select_with`. The first assertion
    /// goes red, which is the whole point of the switch existing.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn the_file_store_switch_is_honoured_on_a_non_dev_channel() {
        let dir = tempfile::tempdir().unwrap();
        // `Stable` specifically: `Dev` already takes the file path
        // unconditionally, so a test on it would pass with the switch deleted.
        let forced = select_with(dir.path(), Channel::Stable, true);
        assert_eq!(
            format!("{forced:?}"),
            "FileStore",
            "the switch must reach the file store on a channel that would otherwise \
             seal in the OS keychain",
        );

        let normal = select_with(dir.path(), Channel::Stable, false);
        assert_ne!(
            format!("{normal:?}"),
            "FileStore",
            "and without it, stable must still use the keychain: a switch that \
             always took the file path would be a silent downgrade",
        );
    }
}
