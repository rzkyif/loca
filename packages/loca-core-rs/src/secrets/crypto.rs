//! The `secrets.enc` container and the master-key encoding.
//!
//! Both are parity surfaces. `secrets.enc` is a 12-byte random nonce followed by
//! the AES-GCM-256 ciphertext with its 16-byte tag appended, which is the layout
//! WebCrypto produces and the layout RustCrypto's `Aead::encrypt` produces, so
//! an existing install's vault decrypts unchanged. The master key is standard
//! base64 (padded, no trailing newline) of 32 bytes, which is what an existing
//! keychain entry already holds.
//!
//! Nothing here formats key material, ciphertext or plaintext into an error. The
//! AEAD crate's error is deliberately unit-like, so dropping it loses nothing,
//! and the base64 error is dropped because it echoes the offending input.

use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use zeroize::Zeroizing;

use crate::error::{internal, Result};

/// AES-GCM's nonce, and the prefix length of every `secrets.enc`.
pub const NONCE_LEN: usize = 12;
/// AES-256.
pub const KEY_LEN: usize = 32;

/// Raw master key, wiped on drop so an unwinding panic cannot leave it behind.
pub type MasterKey = Zeroizing<[u8; KEY_LEN]>;

/// What a stored master-key entry turned out to be. A wrong length is a value
/// rather than an error because the caller's response is to warn and regenerate,
/// while a malformed encoding is an error and must never regenerate.
pub enum StoredKey {
    Valid(MasterKey),
    WrongLength(usize),
}

pub fn generate_master_key() -> MasterKey {
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    OsRng.fill_bytes(key.as_mut_slice());
    key
}

pub fn cipher_for(key: &MasterKey) -> Aes256Gcm {
    Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_slice()))
}

pub fn encode_master_key(key: &MasterKey) -> Zeroizing<String> {
    Zeroizing::new(STANDARD.encode(key.as_slice()))
}

pub fn decode_master_key(encoded: &str) -> Result<StoredKey> {
    let bytes = Zeroizing::new(
        STANDARD
            .decode(encoded.as_bytes())
            .map_err(|_| internal("stored master key is not valid base64"))?,
    );
    if bytes.len() != KEY_LEN {
        return Ok(StoredKey::WrongLength(bytes.len()));
    }
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    key.copy_from_slice(&bytes);
    Ok(StoredKey::Valid(key))
}

/// Encrypt the bag under a fresh nonce. A nonce is drawn per write and never
/// reused: repeating one under the same key is the single crypto mistake here
/// that silently forfeits confidentiality.
pub fn seal(cipher: &Aes256Gcm, plaintext: &[u8]) -> Result<Vec<u8>> {
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| internal("secrets.enc encryption failed"))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Split the container and authenticate it. A tampered byte anywhere, including
/// in the tag, fails here rather than yielding garbage.
pub fn open(cipher: &Aes256Gcm, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    // `get` rather than a slice range: no panic path, whatever the file holds.
    let (nonce, ciphertext) = match (blob.get(..NONCE_LEN), blob.get(NONCE_LEN..)) {
        (Some(nonce), Some(ciphertext)) => (nonce, ciphertext),
        _ => return Err(internal("secrets.enc is shorter than its nonce")),
    };
    let plaintext = cipher
        // Length is guaranteed by the `get(..NONCE_LEN)` above.
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| internal("secrets.enc decryption failed (master key mismatch?)"))?;
    Ok(Zeroizing::new(plaintext))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn key_of(byte: u8) -> MasterKey {
        Zeroizing::new([byte; KEY_LEN])
    }

    #[test]
    fn seal_then_open_round_trips() {
        let cipher = cipher_for(&key_of(1));
        let blob = seal(&cipher, br#"{"k":"v"}"#).unwrap();
        assert_eq!(open(&cipher, &blob).unwrap().as_slice(), br#"{"k":"v"}"#);
    }

    #[test]
    fn container_is_nonce_then_ciphertext_with_tag() {
        let cipher = cipher_for(&key_of(2));
        let plaintext = b"12345";
        let blob = seal(&cipher, plaintext).unwrap();
        // nonce + plaintext + 16-byte GCM tag, no framing of our own.
        assert_eq!(blob.len(), NONCE_LEN + plaintext.len() + 16);
    }

    #[test]
    fn a_flipped_tag_byte_fails_authentication() {
        let cipher = cipher_for(&key_of(3));
        let mut blob = seal(&cipher, b"secret").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        let err = open(&cipher, &blob).unwrap_err();
        assert!(err.message.contains("decryption failed"), "{}", err.message);
    }

    #[test]
    fn the_wrong_key_fails_authentication() {
        let blob = seal(&cipher_for(&key_of(4)), b"secret").unwrap();
        assert!(open(&cipher_for(&key_of(5)), &blob).is_err());
    }

    #[test]
    fn two_writes_of_the_same_plaintext_use_different_nonces() {
        let cipher = cipher_for(&key_of(6));
        let a = seal(&cipher, b"same").unwrap();
        let b = seal(&cipher, b"same").unwrap();
        assert_ne!(a[..NONCE_LEN], b[..NONCE_LEN]);
    }

    #[test]
    fn open_refuses_a_blob_shorter_than_its_nonce() {
        let cipher = cipher_for(&key_of(7));
        assert!(open(&cipher, &[0u8; NONCE_LEN - 1]).is_err());
    }

    #[test]
    fn generated_keys_are_32_bytes_and_not_constant() {
        let a = generate_master_key();
        let b = generate_master_key();
        assert_eq!(a.len(), KEY_LEN);
        assert_ne!(a.as_slice(), b.as_slice());
    }

    #[test]
    fn master_key_encoding_is_padded_standard_base64() {
        let encoded = encode_master_key(&key_of(0));
        // 32 bytes is 44 characters with one pad byte, and no trailing newline:
        // exactly what an existing keychain entry holds.
        assert_eq!(encoded.len(), 44);
        assert!(encoded.ends_with('='));
        match decode_master_key(&encoded).unwrap() {
            StoredKey::Valid(key) => assert_eq!(key.as_slice(), key_of(0).as_slice()),
            StoredKey::WrongLength(n) => panic!("decoded {n} bytes"),
        }
    }

    #[test]
    fn a_short_key_reports_its_length_and_a_malformed_one_errors() {
        match decode_master_key(&STANDARD.encode([1u8; 16])).unwrap() {
            StoredKey::WrongLength(n) => assert_eq!(n, 16),
            StoredKey::Valid(_) => panic!("accepted a 16-byte key"),
        }
        assert!(decode_master_key("not base64!!").is_err());
    }

    // Parity: a `secrets.enc` written by the Deno core must decrypt here. The
    // fixture is base64 of the whole file so the diff stays reviewable, and it
    // was produced by WebCrypto exactly as the Deno core produces one:
    //
    //   deno eval 'const raw = new Uint8Array(32).fill(9);
    //     const k = await crypto.subtle.importKey("raw", raw, {name:"AES-GCM"}, false, ["encrypt"]);
    //     const nonce = crypto.getRandomValues(new Uint8Array(12));
    //     const pt = new TextEncoder().encode(JSON.stringify({"openai-api-key":"sk-test-1234","tls-key":"{\"kty\":\"EC\"}"}));
    //     const ct = new Uint8Array(await crypto.subtle.encrypt({name:"AES-GCM", iv: nonce}, k, pt));
    //     const out = new Uint8Array(nonce.length + ct.length); out.set(nonce); out.set(ct, nonce.length);
    //     console.log(btoa(String.fromCharCode(...out)))'
    #[test]
    fn decrypts_a_secrets_enc_written_by_webcrypto() {
        const FIXTURE: &str = "UQ7v6tZ4oWcFRUGbFV7Ru0AJIv9wQOOezxGZv6qk55ercQ+LTaZVaaskwCc3TwqSxaen1oUei0hTVR5gLodhaSmvHz7lhIrFa1TPWWRGwue23/33HGC0lEh0";
        let blob = STANDARD.decode(FIXTURE).unwrap();
        let plaintext = open(&cipher_for(&Zeroizing::new([9u8; KEY_LEN])), &blob).unwrap();
        assert_eq!(
            std::str::from_utf8(&plaintext).unwrap(),
            r#"{"openai-api-key":"sk-test-1234","tls-key":"{\"kty\":\"EC\"}"}"#
        );
    }
}
