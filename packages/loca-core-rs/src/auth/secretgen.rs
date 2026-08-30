//! Secret generation and the comparisons that must not leak timing.
//!
//! Every value here is drawn from the platform CSPRNG. No distribution sampling
//! is involved, so this takes `getrandom` rather than a full RNG crate.

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::error::{internal, Result};

/// A secret that must not reach a log, an error message, or a panic payload:
/// hand-written `Debug`, wiped on drop, and read only through
/// [`SecretString::expose`] or [`SecretString::into_inner`].
#[derive(Clone)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Take the plaintext, for the one place that has to: serializing the token
    /// into the pairing response the Client stores.
    pub fn into_inner(self) -> String {
        // `Zeroizing` has no `into_inner`: clone out and let it wipe.
        self.0.as_str().to_owned()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<REDACTED>)")
    }
}

/// Fill `dst` from the platform CSPRNG. A failure means the OS could not supply
/// entropy, which becomes an error rather than a weaker source.
pub(crate) fn fill_random(dst: &mut [u8]) -> Result<()> {
    getrandom::fill(dst).map_err(|err| internal(format!("failed to read from the CSPRNG: {err}")))
}

/// A 32-byte bearer token or WS ticket, base64url without padding, which is
/// what every already-paired client holds: 43 characters.
pub(crate) fn random_token() -> Result<SecretString> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    fill_random(bytes.as_mut_slice())?;
    Ok(SecretString::new(URL_SAFE_NO_PAD.encode(bytes.as_slice())))
}

/// Six decimal digits, drawn over 0 to 999999 and left-padded. The modulo over a
/// 32-bit draw is biased by roughly one part in 4000, which is not what makes
/// this safe: the budget around it is (five failed confirmations poison a code,
/// 20 starts per IP per 10 minutes, 10-minute expiry).
pub(crate) fn random_numeric_code() -> Result<String> {
    let mut buf = [0u8; 4];
    fill_random(&mut buf)?;
    let n = u32::from_be_bytes(buf) % 1_000_000;
    Ok(format!("{n:06}"))
}

/// 16 random bytes as lowercase hex: the opaque handle for an in-flight
/// handshake, echoed back on the finish call.
pub(crate) fn random_pake_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes)?;
    Ok(hex::encode(bytes))
}

/// Lowercase hex SHA-256 of a string. Applied to the UTF-8 bytes of the
/// base64url token string, not the 32 raw bytes behind it: hashing those would
/// invalidate every already-paired client.
pub(crate) fn sha256_hex(input: &str) -> String {
    crate::files::sha256_hex(input.as_bytes())
}

/// Constant-time string comparison. The length check short-circuits, which
/// costs nothing here: the only caller compares a fixed-width admin token.
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    bool::from(a.ct_eq(b))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_token_is_43_unpadded_base64url_characters() {
        let token = random_token().unwrap();
        let s = token.expose();
        assert_eq!(s.len(), 43, "32 bytes, base64url, no padding");
        assert!(
            !s.contains('='),
            "padding would not match what clients hold"
        );
        assert!(
            s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "base64url alphabet only: {s}"
        );
        assert_eq!(URL_SAFE_NO_PAD.decode(s).unwrap().len(), 32);
    }

    #[test]
    fn tokens_do_not_repeat() {
        let mut seen = HashSet::new();
        for _ in 0..256 {
            assert!(seen.insert(random_token().unwrap().expose().to_owned()));
        }
    }

    #[test]
    fn a_pairing_code_is_always_six_digits() {
        for _ in 0..2048 {
            let code = random_numeric_code().unwrap();
            assert_eq!(code.len(), 6);
            assert!(code.chars().all(|c| c.is_ascii_digit()), "{code}");
        }
    }

    #[test]
    fn a_pake_id_is_32_lowercase_hex_characters() {
        let id = random_pake_id().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    /// The SHA-256 of the ASCII token text, which is what
    /// `clients.token_hash` holds for an already-paired client.
    #[test]
    fn sha256_hex_matches_the_digest_of_the_string_bytes() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn constant_time_eq_agrees_with_equality() {
        assert!(constant_time_eq("", ""));
        assert!(constant_time_eq("secret-admin", "secret-admin"));
        assert!(!constant_time_eq("secret-admin", "secret-admiN"));
        assert!(!constant_time_eq("secret-admin", "secret-admin "));
        assert!(!constant_time_eq("", "x"));
    }

    #[test]
    fn a_secret_string_never_prints_its_contents() {
        let s = SecretString::new("hunter2".to_owned());
        assert_eq!(format!("{s:?}"), "SecretString(<REDACTED>)");
        assert!(!format!("{s:?}").contains("hunter2"));
        assert_eq!(s.expose(), "hunter2");
        assert_eq!(s.into_inner(), "hunter2");
    }
}
