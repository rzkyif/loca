//! The secrets vault: OS keychain sealing of the master key, with the chmod-600 dotfile fallback.
//!
//! Work package **F5** (`docs/rust-port/registry.md`). This module is that
//! package's owned path, with one recorded exception: M4 added `android.rs` and
//! its `pub mod` line under a bounded grant, to close the plaintext master key
//! on Android.
//!
//! # Two layers
//!
//! [`Vault`] is the bag of named secrets: a single JSON object, AES-GCM-256
//! encrypted under a 32-byte master key and persisted as `secrets.enc`. The
//! whole bag is re-encrypted on every write, which is cheap because the bag is
//! tiny and is the reason per-key crypto is not worth it.
//!
//! [`SecureStore`] is where that master key is kept: the OS keychain where one
//! is usable, and a chmod-600 dotfile under the channel root otherwise. See
//! [`store`] for the selection rules and [`store::LayeredStore`] for why a
//! keychain write is read back before it is believed.
//!
//! # Who calls what
//!
//! - D1's settings routes: [`Vault::list_names`], [`Vault::set`],
//!   [`Vault::delete`]. `delete` returning `false` is the route's `not_found`;
//!   values are never read back over the API, so there is no route for `get`.
//! - F6: [`Vault::get`] / [`Vault::set`] for the sealed TLS private key, and
//!   [`Vault::subscribe`] for the WS hub's name broadcast.
//! - D9's factory reset: [`Vault::clear_all`].
//! - D11: [`Vault::get`] / [`Vault::set`] / [`Vault::delete`] for MCP bearer
//!   tokens and OAuth bags.
//! - D13's uninstall: [`Vault::delete_master_key`].
//! - Boot: [`Vault::warn_if_unreadable`].
//!
//! # Secret hygiene
//!
//! `scrub_secrets` is a backstop for accidents elsewhere and nothing here leans
//! on it. No type in this module derives `Debug`; log lines carry secret names
//! (which the API already returns to any paired client), paths and lengths, but
//! never a value, a key, or a ciphertext; error messages carry no material
//! either; and there is no `unwrap`, `expect`, `panic!` or unguarded index
//! outside `#[cfg(test)]`.

/// The Android sealing layer. Always compiled, on every platform, because the
/// half worth testing has no JNI in it and `cargo test` is host-target: gating
/// it on `target_os = "android"` would leave the migration and the
/// unopenable-envelope path with no coverage anywhere, which is D-117.
pub mod android;
pub mod crypto;
pub mod keychain;
pub mod store;
#[cfg(any(test, feature = "in-memory"))]
pub mod testing;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aes_gcm::Aes256Gcm;
use tokio::sync::{broadcast, Mutex};
use zeroize::Zeroizing;

use crate::config::Channel;
use crate::error::{internal, validation, Result};
use crate::files;
use crate::paths::Paths;

pub use store::SecureStore;

/// The secure-store entry name for the vault master key. Part of the parity
/// surface named in D-012, together with the service name in
/// [`keychain::service_name`] and the base64 encoding in [`crypto`].
pub const MASTER_KEY_ID: &str = "master-key";

/// The dotfile the master key falls back to, `<root>/.master-key`. Exported so
/// the sandbox deny list names exactly this path rather than re-deriving it
/// (D-013).
pub fn master_key_file(root: &Path) -> PathBuf {
    store::key_file(root, MASTER_KEY_ID)
}

/// The transient target of a [`master_key_file`] write, `<root>/.master-key.tmp`.
/// `files::write_secret_atomic` uses a fixed temp name precisely so the deny
/// list can name it; a deny list that misses this one leaves a window where a
/// broadly granted tool worker could read the master key off disk.
pub fn master_key_tmp_file(root: &Path) -> PathBuf {
    store::key_tmp_file(root, MASTER_KEY_ID)
}

/// Name to value. A `BTreeMap` so `list_names` is sorted by construction.
type Bag = BTreeMap<String, String>;

/// How many name-list broadcasts a slow subscriber may fall behind before it
/// starts missing them. A subscriber that lags gets `RecvError::Lagged` and
/// resyncs; it never stalls a vault write.
const CHANGE_CHANNEL_CAPACITY: usize = 16;

/// The encrypted secrets bag. Cheap to clone; every clone shares one cached
/// master key, one mutation lock and one change broadcast.
#[derive(Clone)]
pub struct Vault {
    inner: Arc<Inner>,
}

struct Inner {
    secrets_file: PathBuf,
    store: Arc<dyn SecureStore>,
    /// The master key, loaded or generated once. Behind the same mutex that
    /// serializes generation, so two concurrent readers cannot both decide the
    /// key is absent and both seal a fresh one.
    cipher: Mutex<Option<Arc<Aes256Gcm>>>,
    /// Held across every read-modify-write of the bag. Without it two clients
    /// saving a secret at once both read the same bag and the second rename
    /// clobbers the first, and the fixed-name temp that
    /// `files::write_secret_atomic` uses turns that into a live race over one
    /// file rather than a lost update.
    mutations: Mutex<()>,
    changes: broadcast::Sender<Arc<[String]>>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Vault")
    }
}

impl Vault {
    /// The production wiring: `secrets.enc` under the channel root, and the
    /// store [`store::select`] picks for this platform and channel.
    pub fn new(paths: &Paths, channel: Channel) -> Self {
        Self::with_store(
            paths.secrets_enc_file(),
            store::select(paths.root(), channel),
        )
    }

    /// An explicit store. Tests inject the doubles in [`testing`]; a mobile
    /// frontend that has already registered its own credential store passes it
    /// here so this library never touches keyring-core's process-global
    /// default.
    pub fn with_store(secrets_file: impl Into<PathBuf>, store: Arc<dyn SecureStore>) -> Self {
        let (changes, _) = broadcast::channel(CHANGE_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                secrets_file: secrets_file.into(),
                store,
                cipher: Mutex::new(None),
                mutations: Mutex::new(()),
                changes,
            }),
        }
    }

    pub async fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(self.read_bag().await?.remove(name))
    }

    pub async fn set(&self, name: &str, value: &str) -> Result<()> {
        if name.is_empty() {
            return Err(validation("secret name must be a non-empty string"));
        }
        let _guard = self.inner.mutations.lock().await;
        let mut bag = self.read_bag().await?;
        bag.insert(name.to_string(), value.to_string());
        self.write_bag(&bag).await?;
        self.notify(&bag);
        Ok(())
    }

    /// `false` when there was nothing to delete, which D1's route maps to
    /// `not_found`.
    pub async fn delete(&self, name: &str) -> Result<bool> {
        let _guard = self.inner.mutations.lock().await;
        let mut bag = self.read_bag().await?;
        if bag.remove(name).is_none() {
            return Ok(false);
        }
        self.write_bag(&bag).await?;
        self.notify(&bag);
        Ok(true)
    }

    /// Sorted. Names are not secret: this is what the client renders to show
    /// which secrets are configured.
    pub async fn list_names(&self) -> Result<Vec<String>> {
        Ok(self.read_bag().await?.into_keys().collect())
    }

    /// Drop the entire vault, for the storage view's factory reset. The stored
    /// master key is harmless without ciphertext, so only the file goes.
    pub async fn clear_all(&self) -> Result<()> {
        let _guard = self.inner.mutations.lock().await;
        let file = self.inner.secrets_file.clone();
        blocking(move || match std::fs::remove_file(&file) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        })
        .await?;
        *self.inner.cipher.lock().await = None;
        self.notify(&Bag::new());
        Ok(())
    }

    /// Remove the master key from both the keychain and the dotfile, for
    /// uninstall. Deliberately leaves the cached key in place: dropping it would
    /// let a later write in this same process generate a fresh one and
    /// re-encrypt the vault under it.
    pub async fn delete_master_key(&self) -> Result<()> {
        let store = Arc::clone(&self.inner.store);
        blocking(move || store.delete(MASTER_KEY_ID)).await
    }

    /// Every successful mutation publishes the sorted name list. Names only,
    /// never values.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<[String]>> {
        self.inner.changes.subscribe()
    }

    /// Boot-time integrity check, and strictly non-mutating: it must never
    /// generate a key. A sealed vault with no key readable means the stored
    /// secrets, the TLS private key among them, cannot be decrypted, and the
    /// operator wants to hear that at startup rather than mid-request. Probing
    /// with anything that could regenerate would itself destroy the vault it is
    /// checking.
    pub async fn warn_if_unreadable(&self) {
        match self.sealed_bytes().await {
            Ok(Some(len)) if len > crypto::NONCE_LEN as u64 => {}
            // No vault, an empty one, or a file we cannot stat: nothing sealed
            // to lose.
            _ => return,
        }
        let missing = match self.read_master_key().await {
            Ok(Some(_)) => return,
            Ok(None) => "no master key was found".to_string(),
            Err(err) => format!("the stored master key could not be read ({})", err.message),
        };
        tracing::warn!(
            target: "loca::secrets",
            "secrets.enc exists but {missing}. Stored secrets can't be decrypted; restore \
             the master key or re-enter your secrets in Settings. In dev, preserve the \
             master key across rebuilds.",
        );
    }

    // --- bag io ------------------------------------------------------------

    async fn read_bag(&self) -> Result<Bag> {
        let Some(blob) = self.read_sealed().await? else {
            // No vault yet. Deliberately before any master-key work: reading a
            // fresh install must not generate a key.
            return Ok(Bag::new());
        };
        if blob.len() <= crypto::NONCE_LEN {
            return Ok(Bag::new());
        }
        let cipher = self.cipher().await?;
        let plaintext = crypto::open(&cipher, &blob)?;
        if plaintext.is_empty() {
            return Ok(Bag::new());
        }
        // Not `?` on the serde error: `From<serde_json::Error>` is a 400 for an
        // inbound body, and this is our own state file. The error text is
        // dropped rather than wrapped, because it can quote the input.
        serde_json::from_slice(&plaintext)
            .map_err(|_| internal("secrets.enc decrypted but contained invalid JSON"))
    }

    async fn write_bag(&self, bag: &Bag) -> Result<()> {
        let cipher = self.cipher().await?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(bag).map_err(|_| internal("failed to serialize the secrets bag"))?,
        );
        let blob = crypto::seal(&cipher, &plaintext)?;
        files::write_secret_atomic(self.inner.secrets_file.clone(), blob).await
    }

    async fn read_sealed(&self) -> Result<Option<Vec<u8>>> {
        let file = self.inner.secrets_file.clone();
        blocking(move || match std::fs::read(&file) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(internal(format!(
                "failed to read {}: {err}",
                file.display()
            ))),
        })
        .await
    }

    /// Size of the sealed vault, or `None` if there is no file. Stat rather than
    /// read: the check only needs the length.
    async fn sealed_bytes(&self) -> Result<Option<u64>> {
        let file = self.inner.secrets_file.clone();
        blocking(move || match std::fs::metadata(&file) {
            Ok(meta) => Ok(Some(meta.len())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        })
        .await
    }

    fn notify(&self, bag: &Bag) {
        // Fire and forget: with no subscribers this is an error, and a vault
        // write must not care.
        let _ = self
            .inner
            .changes
            .send(bag.keys().cloned().collect::<Vec<_>>().into());
    }

    // --- master key --------------------------------------------------------

    async fn cipher(&self) -> Result<Arc<Aes256Gcm>> {
        let mut cached = self.inner.cipher.lock().await;
        if let Some(cipher) = cached.as_ref() {
            return Ok(Arc::clone(cipher));
        }
        let key = match self.read_master_key().await? {
            Some(key) => key,
            None => {
                let key = crypto::generate_master_key();
                let store = Arc::clone(&self.inner.store);
                let encoded = crypto::encode_master_key(&key);
                blocking(move || store.set(MASTER_KEY_ID, &encoded)).await?;
                tracing::info!(target: "loca::secrets", "generated a new vault master key");
                key
            }
        };
        let cipher = Arc::new(crypto::cipher_for(&key));
        *cached = Some(Arc::clone(&cipher));
        Ok(cipher)
    }

    /// The stored key, or `None` if there is none to load. Never writes.
    async fn read_master_key(&self) -> Result<Option<crypto::MasterKey>> {
        let store = Arc::clone(&self.inner.store);
        let Some(encoded) = blocking(move || store.get(MASTER_KEY_ID)).await? else {
            return Ok(None);
        };
        if encoded.is_empty() {
            return Ok(None);
        }
        match crypto::decode_master_key(&encoded)? {
            crypto::StoredKey::Valid(key) => Ok(Some(key)),
            // Treated as absent, which regenerates and orphans whatever was
            // sealed. That is the lesser evil against running with a key we
            // know is wrong, and it is what the length check has always done.
            crypto::StoredKey::WrongLength(len) => {
                tracing::warn!(
                    target: "loca::secrets",
                    "the stored master key has {len} bytes; expected {}. Ignoring it and \
                     regenerating (any existing vault becomes unreadable).",
                    crypto::KEY_LEN,
                );
                Ok(None)
            }
        }
    }
}

/// Run a store or filesystem call off the async runtime.
///
/// The closure holds key material, so a panic inside it must not have its
/// payload formatted: `JoinError`'s Display carries the panic message, and the
/// message of a panic raised while holding a decoded key can carry the key. The
/// join error is dropped for that reason, not out of laziness.
async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(_) => Err(internal("a secure store task failed")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::testing::{MemoryStore, WriteOnlyStore};
    use super::*;

    struct Fixture {
        dir: tempfile::TempDir,
        store: Arc<dyn SecureStore>,
    }

    impl Fixture {
        /// A vault whose keychain works.
        fn sealed() -> Self {
            Self::with(Arc::new(MemoryStore::new()))
        }

        /// A vault with no keychain at all, so the master key lands in the
        /// dotfile. This is the dev-channel and Android shape.
        fn file_backed() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(store::FileStore::new(dir.path()));
            Self { dir, store }
        }

        fn with(store: Arc<dyn SecureStore>) -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
                store,
            }
        }

        /// A fresh handle over the same directory and the same store: the next
        /// boot, with nothing cached.
        fn reopen(&self) -> Vault {
            Vault::with_store(self.secrets_file(), Arc::clone(&self.store))
        }

        fn vault(&self) -> Vault {
            self.reopen()
        }

        fn secrets_file(&self) -> PathBuf {
            self.dir.path().join("secrets.enc")
        }

        fn master_key_file(&self) -> PathBuf {
            super::master_key_file(self.dir.path())
        }
    }

    #[tokio::test]
    async fn set_then_get_round_trips() {
        let f = Fixture::sealed();
        let vault = f.vault();
        vault.set("openai-api-key", "sk-test-1234").await.unwrap();
        assert_eq!(
            vault.get("openai-api-key").await.unwrap().as_deref(),
            Some("sk-test-1234")
        );
    }

    #[tokio::test]
    async fn get_of_a_name_that_was_never_set_is_none() {
        let f = Fixture::sealed();
        assert_eq!(f.vault().get("absent").await.unwrap(), None);
    }

    #[tokio::test]
    async fn setting_the_same_name_again_overwrites() {
        let f = Fixture::sealed();
        let vault = f.vault();
        vault.set("k", "v1").await.unwrap();
        vault.set("k", "v2").await.unwrap();
        assert_eq!(vault.get("k").await.unwrap().as_deref(), Some("v2"));
    }

    #[tokio::test]
    async fn list_names_is_sorted() {
        let f = Fixture::sealed();
        let vault = f.vault();
        for name in ["z-secret", "a-secret", "m-secret"] {
            vault.set(name, "1").await.unwrap();
        }
        assert_eq!(
            vault.list_names().await.unwrap(),
            vec!["a-secret", "m-secret", "z-secret"]
        );
    }

    #[tokio::test]
    async fn delete_reports_whether_there_was_anything_to_delete() {
        let f = Fixture::sealed();
        let vault = f.vault();
        vault.set("doomed", "v").await.unwrap();
        assert!(vault.delete("doomed").await.unwrap());
        assert!(!vault.delete("doomed").await.unwrap());
        assert_eq!(vault.get("doomed").await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_empty_name_is_rejected() {
        let f = Fixture::sealed();
        let err = f.vault().set("", "v").await.unwrap_err();
        assert!(err.message.contains("non-empty"), "{}", err.message);
    }

    #[tokio::test]
    async fn the_bag_survives_a_reopen() {
        let f = Fixture::sealed();
        f.vault().set("k", "v").await.unwrap();
        assert_eq!(f.reopen().get("k").await.unwrap().as_deref(), Some("v"));
    }

    #[tokio::test]
    async fn clear_all_drops_the_vault_and_the_next_write_starts_clean() {
        let f = Fixture::sealed();
        let vault = f.vault();
        vault.set("k", "v").await.unwrap();
        vault.clear_all().await.unwrap();
        assert!(!f.secrets_file().exists());
        assert_eq!(vault.list_names().await.unwrap(), Vec::<String>::new());
        // Tolerates a vault that is already gone.
        vault.clear_all().await.unwrap();
        vault.set("k2", "v2").await.unwrap();
        assert_eq!(vault.get("k2").await.unwrap().as_deref(), Some("v2"));
    }

    #[tokio::test]
    async fn reading_a_fresh_install_does_not_generate_a_master_key() {
        let f = Fixture::file_backed();
        let vault = f.vault();
        assert_eq!(vault.get("anything").await.unwrap(), None);
        assert_eq!(vault.list_names().await.unwrap(), Vec::<String>::new());
        assert!(!f.master_key_file().exists());
    }

    #[tokio::test]
    async fn an_empty_or_truncated_vault_reads_as_an_empty_bag() {
        let f = Fixture::sealed();
        for len in [0usize, crypto::NONCE_LEN] {
            std::fs::write(f.secrets_file(), vec![0u8; len]).unwrap();
            assert_eq!(f.vault().list_names().await.unwrap(), Vec::<String>::new());
        }
    }

    #[tokio::test]
    async fn tampered_ciphertext_is_rejected_and_the_error_carries_no_material() {
        let f = Fixture::sealed();
        f.vault().set("k", "hunter2").await.unwrap();
        let mut blob = std::fs::read(f.secrets_file()).unwrap();
        // The GCM tag is the last 16 bytes, so any flip there fails
        // authentication.
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        std::fs::write(f.secrets_file(), &blob).unwrap();
        let err = f.reopen().get("k").await.unwrap_err();
        assert!(err.message.contains("decryption failed"), "{}", err.message);
        assert!(!err.message.contains("hunter2"), "{}", err.message);
    }

    #[tokio::test]
    async fn two_writes_of_the_same_value_produce_different_nonces() {
        let f = Fixture::sealed();
        let vault = f.vault();
        vault.set("k", "same-value").await.unwrap();
        let a = std::fs::read(f.secrets_file()).unwrap();
        vault.set("k", "same-value").await.unwrap();
        let b = std::fs::read(f.secrets_file()).unwrap();
        assert_ne!(a[..crypto::NONCE_LEN], b[..crypto::NONCE_LEN]);
    }

    #[tokio::test]
    async fn concurrent_writers_do_not_lose_updates() {
        let f = Fixture::sealed();
        let vault = f.vault();
        let mut tasks = Vec::new();
        for i in 0..16 {
            let vault = vault.clone();
            tasks.push(tokio::spawn(async move {
                vault.set(&format!("k{i}"), "v").await
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        assert_eq!(vault.list_names().await.unwrap().len(), 16);
    }

    #[tokio::test]
    async fn every_mutation_broadcasts_the_sorted_name_list() {
        let f = Fixture::sealed();
        let vault = f.vault();
        let mut changes = vault.subscribe();
        vault.set("b", "1").await.unwrap();
        vault.set("a", "1").await.unwrap();
        vault.delete("b").await.unwrap();
        vault.clear_all().await.unwrap();
        assert_eq!(&*changes.recv().await.unwrap(), ["b".to_string()]);
        assert_eq!(
            &*changes.recv().await.unwrap(),
            ["a".to_string(), "b".to_string()]
        );
        assert_eq!(&*changes.recv().await.unwrap(), ["a".to_string()]);
        assert!(changes.recv().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_dropped_subscriber_does_not_fail_a_write() {
        let f = Fixture::sealed();
        let vault = f.vault();
        drop(vault.subscribe());
        vault.set("k", "v").await.unwrap();
    }

    #[tokio::test]
    async fn a_lost_master_key_is_reported_without_regenerating_one() {
        let f = Fixture::file_backed();
        f.vault().set("k", "v").await.unwrap();
        assert!(f.master_key_file().exists());
        // A dev rebuild that dropped the key.
        std::fs::remove_file(f.master_key_file()).unwrap();

        f.reopen().warn_if_unreadable().await;

        // Non-mutating: regenerating here would orphan the still-sealed vault.
        assert!(!f.master_key_file().exists());
    }

    #[tokio::test]
    async fn warn_if_unreadable_is_a_no_op_without_a_vault() {
        let f = Fixture::file_backed();
        f.reopen().warn_if_unreadable().await;
        assert!(!f.master_key_file().exists());
    }

    #[tokio::test]
    async fn a_wrong_length_master_key_regenerates_and_a_malformed_one_errors() {
        let f = Fixture::sealed();
        f.store.set(MASTER_KEY_ID, "QUJD").unwrap(); // 3 bytes
        f.vault().set("k", "v").await.unwrap();
        assert_eq!(f.reopen().get("k").await.unwrap().as_deref(), Some("v"));

        f.store.set(MASTER_KEY_ID, "not base64!!").unwrap();
        assert!(f.reopen().list_names().await.is_err());
    }

    #[tokio::test]
    async fn the_master_key_is_stored_as_44_characters_of_base64() {
        let f = Fixture::sealed();
        f.vault().set("k", "v").await.unwrap();
        let stored = f.store.get(MASTER_KEY_ID).unwrap().unwrap();
        assert_eq!(stored.len(), 44);
        assert!(!stored.ends_with('\n'));
    }

    // The whole point of the read-back verification, end to end: a keychain
    // that swallows the write must not cost the vault.
    #[tokio::test]
    async fn a_swallowed_keychain_write_leaves_the_vault_readable_across_boots() {
        let keychain = Arc::new(WriteOnlyStore::new());
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(store::LayeredStore::new(
            keychain.clone(),
            store::FileStore::new(dir.path()),
        ));
        let secrets_file = dir.path().join("secrets.enc");
        let open = || {
            Vault::with_store(
                secrets_file.clone(),
                Arc::clone(&store) as Arc<dyn SecureStore>,
            )
        };

        open().set("k", "v1").await.unwrap();
        assert!(keychain.swallowed() >= 1, "the seal was never attempted");
        assert!(master_key_file(dir.path()).exists());

        // The next boot loads from the file, retries the still-lying keychain,
        // and must not delete the file it depends on.
        assert_eq!(open().get("k").await.unwrap().as_deref(), Some("v1"));
        assert!(master_key_file(dir.path()).exists());
        assert_eq!(open().get("k").await.unwrap().as_deref(), Some("v1"));
    }

    #[tokio::test]
    async fn delete_master_key_clears_the_store() {
        let f = Fixture::sealed();
        f.vault().set("k", "v").await.unwrap();
        assert!(f.store.get(MASTER_KEY_ID).unwrap().is_some());
        let vault = f.vault();
        vault.delete_master_key().await.unwrap();
        vault.delete_master_key().await.unwrap();
        assert_eq!(f.store.get(MASTER_KEY_ID).unwrap(), None);
    }

    #[test]
    fn the_dotfile_and_its_temp_are_named_from_one_place() {
        let root = Path::new("/x/core");
        assert_eq!(master_key_file(root), Path::new("/x/core/.master-key"));
        assert_eq!(
            master_key_tmp_file(root),
            Path::new("/x/core/.master-key.tmp")
        );
    }

    #[test]
    fn debug_reveals_nothing() {
        let vault = Vault::with_store("/x/secrets.enc", Arc::new(MemoryStore::new()));
        assert_eq!(format!("{vault:?}"), "Vault");
    }
}
