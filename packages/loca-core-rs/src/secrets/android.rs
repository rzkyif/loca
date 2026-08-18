//! Sealing the master key on Android, where there is no OS credential store.
//!
//! Every other platform hands [`super::store::LayeredStore`] a real keychain.
//! Android has none wired, so until this module the vault's master key rested
//! in the app-private directory in plaintext, protected by the app sandbox
//! alone. The master key unseals `secrets.enc`, and `secrets.enc` holds the TLS
//! private key, so what is at stake is the pairing trust root rather than a
//! convenience.
//!
//! # The split, and why it is here rather than in the frontend
//!
//! The KeyStore itself is reachable only through the JVM, so the cryptography
//! lives in `SecretBox.kt` in the Android app and the JNI call lives in the
//! Tauri crate, which is the crate that owns the Android dependency block.
//! Everything in THIS file is the part that has no JNI in it: the on-disk
//! framing, the migration from a plaintext file, and what happens when an
//! envelope cannot be opened. That split is deliberate and it is D-117's
//! lesson: `cargo test` is host-target, so any logic left on the far side of a
//! `#[cfg(target_os = "android")]` has no coverage at all. The rules with an
//! ordering or a failure mode to get wrong are all on this side, driven by a
//! [`SecretBox`] double.
//!
//! # The file format, which this module owns outright
//!
//! One path, the same `.master-key` the plaintext build used, holding either:
//!
//! - `LOCAK1:<base64 of iv||ciphertext+tag>`, or
//! - the bare plaintext value an older build wrote.
//!
//! Reusing the path rather than adding a `.enc` sibling is not tidiness. The
//! sandbox deny list names `secrets::master_key_file` and
//! `secrets::master_key_tmp_file` exactly (`extensions/sandbox.rs:533-537`), so
//! a second path would need a matching entry, and a deny list that misses a
//! secret is the failure D-013 filed. One path means the existing entries keep
//! covering it. (No extension worker runs on Android at all today, since
//! `extensions::worker::spawner` is gated off both mobile targets, so the deny
//! list is unreachable there; the point is that whoever changes that later
//! inherits the coverage rather than having to notice its absence.)
//!
//! Keeping the path also makes migration fall out for free: a read that finds
//! no version marker has found a plaintext key from an older build, returns it,
//! and re-seals it in place.
//!
//! The marker lives here rather than inside the ciphertext so that there is NO
//! constant shared across the JNI boundary. `SecretBox.kt` deals in raw bytes
//! and owns no format, so the two halves cannot drift apart.

use std::path::PathBuf;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use super::store::{FileStore, SecureStore};
use crate::error::{internal, Result};

/// Version marker on a sealed value, without its separator.
pub const SEALED_MAGIC: &str = "LOCAK1";
/// The full framed prefix, `LOCAK1:`.
pub const SEALED_PREFIX: &str = "LOCAK1:";

/// AES-GCM sealing backed by a key this process cannot extract.
///
/// Blocking, like [`SecureStore`] and for the same reason: the implementation
/// is a JNI round trip into the platform provider, which the vault already runs
/// inside `spawn_blocking`.
///
/// `Debug` is required and implementations print the type name and nothing
/// else. A derive would put an alias, and one refactor later a plaintext, into
/// any `{:?}` that reaches a log line or a panic message. That is D-017's rule
/// applied before there is an error type to get it wrong with.
pub trait SecretBox: Send + Sync + std::fmt::Debug {
    /// `iv || ciphertext+tag`. Generates the key on first use.
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>>;
    /// The inverse. Fails when the key is gone, which is reachable without an
    /// attacker: a device-wide credential reset destroys KeyStore keys.
    fn unseal(&self, sealed: &[u8]) -> Result<Vec<u8>>;
}

/// Frame a sealed blob for the file.
pub fn frame(sealed: &[u8]) -> String {
    format!("{SEALED_PREFIX}{}", BASE64.encode(sealed))
}

/// What a stored value turned out to be.
///
/// **Three outcomes, not two, and the third is why this is an enum.** An
/// earlier version returned `Option<Vec<u8>>`, so a value carrying an UNKNOWN
/// marker (`LOCAK2:...`) came back as `None`, which the only caller reads as
/// "plaintext from an older build". The consequence was not a refusal: the
/// ciphertext was handed back AS THE MASTER KEY, the envelope was destroyed by a
/// `LOCAK1` re-seal over it, quarantine was skipped, and the log claimed a
/// plaintext migration that never happened. The marker exists to make a format
/// change detectable and instead made one silent and destructive.
///
/// Collapsing "unknown" into "absent" IS that bug, so they are separate variants
/// and the compiler will not let a caller forget the third.
#[derive(Debug, PartialEq, Eq)]
pub enum Framing {
    /// `LOCAK1:` and a decodable body.
    Sealed(Vec<u8>),
    /// No marker at all: a plaintext key written before this module existed.
    Plain,
    /// A marker this build does not know. Carries it for the log line.
    UnknownMarker(String),
}

/// Read a stored value's framing.
///
/// The marker is everything before the first `:`, when that run is non-empty and
/// entirely `[A-Z0-9]`. A legacy plaintext master key is base64 of 32 bytes
/// (`A-Za-z0-9+/=`) and can never contain a colon, so this cannot misread one as
/// a marker; the Client's token map is JSON and starts with `{`, so it cannot
/// either.
pub fn unframe(text: &str) -> Result<Framing> {
    let text = text.trim();
    match marker_of(text) {
        None => Ok(Framing::Plain),
        Some(SEALED_MAGIC) => BASE64
            .decode(&text[SEALED_PREFIX.len()..])
            .map(Framing::Sealed)
            .map_err(|err| internal(format!("sealed value is not valid base64: {err}"))),
        Some(other) => Ok(Framing::UnknownMarker(other.to_string())),
    }
}

/// The version marker of a framed value, or `None` when it carries none.
///
/// Mirrored by `commands/keychain.rs` in the Client, which cannot depend on this
/// crate off mobile. Each suite drives the SAME input table
/// (`the_two_stores_classify_the_same_bytes_the_same_way`) so the two copies
/// cannot drift, which is the concrete answer to "one on-disk format, two
/// implementations that disagreed about it".
pub fn marker_of(text: &str) -> Option<&str> {
    let idx = text.find(':')?;
    let marker = &text[..idx];
    let ok = !marker.is_empty()
        && marker
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    ok.then_some(marker)
}

/// [`FileStore`] with every value sealed on the way out and opened on the way
/// in, migrating a plaintext file the first time it is read.
pub struct SealedStore {
    file: FileStore,
    boxed: Arc<dyn SecretBox>,
}

impl SealedStore {
    pub fn new(root: impl Into<PathBuf>, boxed: Arc<dyn SecretBox>) -> Self {
        Self {
            file: FileStore::new(root),
            boxed,
        }
    }
}

impl std::fmt::Debug for SealedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealedStore")
    }
}

impl SecureStore for SealedStore {
    /// Three outcomes, and the third is the one worth reading.
    ///
    /// Framed and openable: the value. Unframed: a plaintext key from an older
    /// build, returned and re-sealed in place. Framed and NOT openable: treated
    /// as **absent**, with the file renamed aside.
    ///
    /// That last choice is D-079's shape. The state is reachable without an
    /// attacker (a credential reset destroys the KeyStore key while leaving the
    /// file), and the alternatives are worse in both directions: propagating
    /// the error fails the boot, so a rotated key becomes a phone that will not
    /// start; deleting the file destroys the only evidence a support case would
    /// have. Absent means the vault regenerates, the user re-pairs, and the
    /// bytes are still on disk if anyone asks.
    fn get(&self, key: &str) -> Result<Option<String>> {
        let Some(stored) = self.file.get(key)? else {
            return Ok(None);
        };
        let sealed = match unframe(&stored)? {
            Framing::Sealed(sealed) => sealed,
            Framing::Plain => {
                tracing::info!(
                    target: "loca::secure-store",
                    "\"{key}\" was stored in plaintext by an earlier build; sealing it now",
                );
                // A failure to re-seal is not a failure to READ. The value is in
                // hand and the caller needs it; the next write tries again.
                if let Err(err) = self.set(key, &stored) {
                    tracing::warn!(
                        target: "loca::secure-store",
                        "could not seal the existing \"{key}\": {}", err.message,
                    );
                }
                return Ok(Some(stored));
            }
            // The case the marker exists for. It must NOT reach the migration
            // branch above: doing so returns the ciphertext as the key and
            // overwrites it, which is the opposite of detecting a format change.
            Framing::UnknownMarker(marker) => {
                self.quarantine(
                    key,
                    &format!("it is framed as \"{marker}\", which this build cannot read"),
                );
                return Ok(None);
            }
        };
        match self.boxed.unseal(&sealed) {
            Ok(plain) => match String::from_utf8(plain) {
                Ok(text) => Ok(Some(text).filter(|t| !t.is_empty())),
                Err(_) => {
                    self.quarantine(key, "the opened value was not valid UTF-8");
                    Ok(None)
                }
            },
            Err(err) => {
                // `err.message`, never `{err:?}`: D-017's rule, and the reason it
                // exists is that a Debug rendering of a crypto error is exactly
                // where a byte buffer leaks into a log.
                self.quarantine(key, &err.message);
                Ok(None)
            }
        }
    }

    /// Seal and write. **A seal failure falls back to plaintext with a loud
    /// warning rather than propagating.**
    ///
    /// F-K found the asymmetry: swapping this store in for `LayeredStore` on
    /// Android removed the one fallback the desktop path has. `LayeredStore`
    /// answers a refusing keychain by writing the dotfile and warning
    /// (`store.rs`, `seal_verified`); this propagated with `?`, and the master
    /// key's first write is on the boot path, so a provider hiccup on a fresh
    /// install was a phone that does not start.
    ///
    /// Bricking is the outcome this whole failure policy was written to avoid,
    /// and D-125 already settled the shape for a degraded secret store: the
    /// mitigation is LOUDNESS, not restriction. So the value lands, the warning
    /// says exactly what happened, and the next successful write re-seals it
    /// (the read path migrates any unframed value it finds).
    fn set(&self, key: &str, value: &str) -> Result<()> {
        match self.boxed.seal(value.as_bytes()) {
            Ok(sealed) => self.file.set(key, &frame(&sealed)),
            Err(err) => {
                tracing::warn!(
                    target: "loca::secure-store",
                    "the keystore refused to seal \"{key}\" ({}); storing it in the \
                     app-private directory in PLAINTEXT so the app still starts. \
                     It is re-sealed on the next successful write.",
                    err.message,
                );
                self.file.set(key, value)
            }
        }
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.file.delete(key)
    }
}

impl SealedStore {
    /// Move an unopenable file aside, once, and say why. Best effort by design:
    /// failing to rename must not turn an unreadable secret into an unbootable
    /// core, and the caller has already decided to treat the entry as absent.
    fn quarantine(&self, key: &str, reason: &str) {
        let path = self.file.path(key);
        let mut aside = path.clone().into_os_string();
        aside.push(".unreadable");
        let aside = PathBuf::from(aside);
        tracing::warn!(
            target: "loca::secure-store",
            "\"{key}\" is sealed with a keystore key this device no longer has ({reason}); \
             treating it as absent and moving the file to {}. Paired clients will need to \
             pair again.",
            aside.display(),
        );
        if let Err(err) = std::fs::rename(&path, &aside) {
            tracing::warn!(
                target: "loca::secure-store",
                "could not move the unreadable \"{key}\" aside: {err}",
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A [`SecretBox`] double. XOR is not encryption and is not pretending to
    /// be: what these tests exercise is the framing, the migration and the
    /// failure handling, none of which depends on the cipher. The real box is
    /// JNI and is unreachable from a host test binary at all (D-117).
    #[derive(Default)]
    struct FakeBox {
        fail_unseal: Mutex<bool>,
        fail_seal: Mutex<bool>,
    }

    impl std::fmt::Debug for FakeBox {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("FakeBox")
        }
    }

    impl SecretBox for FakeBox {
        fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
            if *self.fail_seal.lock().unwrap() {
                return Err(internal("keystore refused"));
            }
            let mut out = vec![0xAA; 12];
            out.extend(plaintext.iter().map(|b| b ^ 0x5A));
            Ok(out)
        }
        fn unseal(&self, sealed: &[u8]) -> Result<Vec<u8>> {
            if *self.fail_unseal.lock().unwrap() {
                return Err(internal("no keystore key for the alias"));
            }
            if sealed.len() <= 12 {
                return Err(internal("too short"));
            }
            Ok(sealed[12..].iter().map(|b| b ^ 0x5A).collect())
        }
    }

    fn store(dir: &tempfile::TempDir) -> (SealedStore, Arc<FakeBox>) {
        let boxed = Arc::new(FakeBox::default());
        (
            SealedStore::new(dir.path(), Arc::clone(&boxed) as Arc<dyn SecretBox>),
            boxed,
        )
    }

    #[test]
    fn a_value_round_trips_through_the_seal() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = store(&dir);
        store.set("master-key", "the-key").unwrap();
        assert_eq!(store.get("master-key").unwrap(), Some("the-key".into()));
    }

    /// The property that makes this module worth having. Asserting the ROUND
    /// TRIP alone would pass against a store that wrote plaintext, so the file
    /// itself is read back and checked for the absence of the value.
    #[test]
    fn what_lands_on_disk_is_sealed_rather_than_the_value() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = store(&dir);
        store.set("master-key", "the-key").unwrap();
        let raw = std::fs::read_to_string(dir.path().join(".master-key")).unwrap();
        assert!(raw.starts_with(SEALED_PREFIX), "not framed: {raw}");
        assert!(!raw.contains("the-key"), "the plaintext survived: {raw}");
    }

    #[test]
    fn a_plaintext_file_from_an_older_build_is_read_and_then_sealed_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = store(&dir);
        let path = dir.path().join(".master-key");
        std::fs::write(&path, "legacy-key").unwrap();

        // The read still answers, which is what stops an upgrade losing a vault.
        assert_eq!(store.get("master-key").unwrap(), Some("legacy-key".into()));
        // And the plaintext is gone afterwards, which is the migration.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with(SEALED_PREFIX), "not migrated: {raw}");
        assert!(!raw.contains("legacy-key"));
        assert_eq!(store.get("master-key").unwrap(), Some("legacy-key".into()));
    }

    /// A migration that cannot seal must still hand back the value: refusing
    /// would turn a working install into a lost vault on the one boot where the
    /// keystore happens to be unavailable.
    #[test]
    fn a_plaintext_file_is_still_readable_when_sealing_it_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (store, boxed) = store(&dir);
        std::fs::write(dir.path().join(".master-key"), "legacy-key").unwrap();
        *boxed.fail_seal.lock().unwrap() = true;
        assert_eq!(store.get("master-key").unwrap(), Some("legacy-key".into()));
    }

    #[test]
    fn an_envelope_the_keystore_can_no_longer_open_reads_as_absent_and_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let (store, boxed) = store(&dir);
        store.set("master-key", "the-key").unwrap();

        *boxed.fail_unseal.lock().unwrap() = true;
        assert_eq!(store.get("master-key").unwrap(), None, "must not propagate");
        assert!(!dir.path().join(".master-key").exists());
        assert!(dir.path().join(".master-key.unreadable").exists());
    }

    #[test]
    fn a_missing_entry_is_absent_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = store(&dir);
        assert_eq!(store.get("master-key").unwrap(), None);
    }

    #[test]
    fn delete_removes_the_file_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = store(&dir);
        store.set("master-key", "the-key").unwrap();
        store.delete("master-key").unwrap();
        store.delete("master-key").unwrap();
        assert_eq!(store.get("master-key").unwrap(), None);
    }

    /// An unknown marker is refused rather than guessed at. Without this a
    /// future `LOCAK2:` would be base64-decoded as though it were `LOCAK1:`
    /// and handed to the cipher, which turns a format change into a tag failure
    /// and then into a silently quarantined vault.
    /// **The previous version of this test asserted the bug.** It called
    /// `unframe` directly and asserted `Ok(None)` for `LOCAK2:`, and `Ok(None)`
    /// was the value that CAUSED the behaviour the doc comment said was
    /// prevented: the caller reads it as "plaintext", so the ciphertext came
    /// back as the master key and was overwritten. It pinned a helper's return
    /// value while the doc claimed a system property, and nothing connected the
    /// two.
    ///
    /// So this drives `SealedStore::get` on a real file and asserts the property
    /// itself: nothing is returned, the envelope is preserved as evidence, and
    /// it is NOT re-sealed over.
    #[test]
    fn a_value_framed_by_a_newer_build_is_quarantined_rather_than_read_as_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = store(&dir);
        let path = dir.path().join(".master-key");
        std::fs::write(&path, "LOCAK2:YWJjZA==").unwrap();

        assert_eq!(
            store.get("master-key").unwrap(),
            None,
            "a newer format must not be handed back as the key",
        );
        assert!(!path.exists(), "the unreadable envelope was left in place");
        let aside = std::fs::read_to_string(dir.path().join(".master-key.unreadable")).unwrap();
        assert_eq!(
            aside, "LOCAK2:YWJjZA==",
            "the original envelope must survive as evidence, not be re-sealed over",
        );
    }

    #[test]
    fn framing_classifies_sealed_plaintext_and_unknown_markers_apart() {
        assert_eq!(
            unframe("LOCAK2:abcd").unwrap(),
            Framing::UnknownMarker("LOCAK2".into()),
        );
        assert_eq!(unframe("plain").unwrap(), Framing::Plain);
        // A legacy master key is base64 of 32 bytes and carries no colon, so it
        // must never read as a marker.
        assert_eq!(
            unframe("YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY3OA==").unwrap(),
            Framing::Plain,
        );
        // A colon that is not a marker (lowercase, punctuation) is plaintext.
        assert_eq!(unframe("http://x").unwrap(), Framing::Plain);
        assert_eq!(unframe(":abcd").unwrap(), Framing::Plain);
        assert!(unframe("LOCAK1:not base64!!").is_err());
        assert_eq!(
            unframe(&frame(b"hello")).unwrap(),
            Framing::Sealed(b"hello".to_vec())
        );
    }

    /// The two on-disk stores share one format by design, and F-F found them
    /// disagreeing about the same bytes: this one accepted `LOCAK2:` as a
    /// secret while the Client's hard-errored forever. The Client cannot depend
    /// on this crate off mobile, so the classifier is mirrored there and THIS
    /// TABLE is what keeps the two copies honest. The Client's suite drives the
    /// identical inputs; changing one without the other turns one of them red.
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

    /// F-K: the fresh-install write had no test in either direction, and it is
    /// the one remaining shape of "a phone that cannot start". The desktop
    /// `LayeredStore` degrades here; this store used to propagate.
    #[test]
    fn a_keystore_that_refuses_the_first_write_degrades_loudly_instead_of_failing_the_boot() {
        let dir = tempfile::tempdir().unwrap();
        let (store, boxed) = store(&dir);
        *boxed.fail_seal.lock().unwrap() = true;

        store
            .set("master-key", "fresh-key")
            .expect("a refusing keystore must not fail the write");
        // Readable afterwards, which is what stops the boot failing.
        assert_eq!(store.get("master-key").unwrap(), Some("fresh-key".into()));

        // And it is genuinely plaintext, not silently lost: the warning is the
        // mitigation, so the test asserts the state the warning describes.
        let raw = std::fs::read_to_string(dir.path().join(".master-key")).unwrap();
        assert_eq!(raw.trim(), "fresh-key");

        // The recovery: once the keystore works, the next read re-seals it.
        *boxed.fail_seal.lock().unwrap() = false;
        assert_eq!(store.get("master-key").unwrap(), Some("fresh-key".into()));
        let raw = std::fs::read_to_string(dir.path().join(".master-key")).unwrap();
        assert!(raw.starts_with(SEALED_PREFIX), "not re-sealed: {raw}");
    }
}
