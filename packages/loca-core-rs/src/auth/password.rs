//! Admin-password hashing.
//!
//! The admin password lets an already-paired client mint pairing codes and
//! revoke other devices without reading the on-disk admin token. It is a
//! memorable, low-entropy secret, so it is hashed with argon2id under a
//! per-install random salt: a leaked hash file resists offline cracking, and
//! each online guess costs real work.
//!
//! The verify path rate-limits **before** calling here, so argon2id cannot be
//! turned into a CPU denial of service.
//!
//! The on-disk format is not PHC:
//!
//! ```text
//! argon2id$m=<KiB>,t=<iters>,p=<lanes>$<saltBase64>$<hashBase64>
//! ```
//!
//! Standard-alphabet base64 **with** padding, no `v=` field, and no leading `$`,
//! so the `argon2` crate's `PasswordHash` parser cannot read it. Parameters come
//! from the stored string; the constants below apply only to a new hash.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use super::secretgen::fill_random;
use crate::error::{internal, Result};

/// OWASP's minimum argon2id profile: 19 MiB, 2 iterations, 1 lane.
const ARGON_M_KIB: u32 = 19 * 1024;
const ARGON_T: u32 = 2;
const ARGON_P: u32 = 1;
const HASH_LEN: usize = 32;
const SALT_LEN: usize = 16;

/// argon2id, RFC 9106 version 0x13, no secret key and no associated data, which
/// is what every stored hash was produced with.
fn derive(
    password: &str,
    salt: &[u8],
    m: u32,
    t: u32,
    p: u32,
) -> std::result::Result<Zeroizing<[u8; HASH_LEN]>, argon2::Error> {
    let params = Params::new(m, t, p, Some(HASH_LEN))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; HASH_LEN]);
    argon.hash_password_into(password.as_bytes(), salt, out.as_mut_slice())?;
    Ok(out)
}

/// Hash a password into the on-disk format string, with a fresh salt.
pub(crate) fn hash_admin_password(password: &str) -> Result<String> {
    let mut salt = [0u8; SALT_LEN];
    fill_random(&mut salt)?;
    let hash = derive(password, &salt, ARGON_M_KIB, ARGON_T, ARGON_P)
        // Mapped rather than forwarded so the password can never reach the
        // message if the crate's Display changes.
        .map_err(|_| internal("failed to hash the admin password"))?;
    Ok(format!(
        "argon2id$m={ARGON_M_KIB},t={ARGON_T},p={ARGON_P}${}${}",
        STANDARD.encode(salt),
        STANDARD.encode(hash.as_slice())
    ))
}

/// Verify a password against a stored format string.
///
/// Total: false on any parse failure or mismatch, so a corrupt file locks the
/// password path rather than turning into a 500. Constant-time comparison.
pub(crate) fn verify_admin_password(password: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.trim().split('$').collect();
    if parts.len() != 4 || parts[0] != "argon2id" {
        return false;
    }
    let Some((m, t, p)) = parse_params(parts[1]) else {
        return false;
    };
    let (Ok(salt), Ok(expected)) = (STANDARD.decode(parts[2]), STANDARD.decode(parts[3])) else {
        return false;
    };
    if expected.len() != HASH_LEN {
        return false;
    }
    let Ok(actual) = derive(password, &salt, m, t, p) else {
        return false;
    };
    bool::from(actual.as_slice().ct_eq(&expected))
}

/// Parse `m=<n>,t=<n>,p=<n>`. Unknown keys are ignored; a missing or
/// non-positive `m`, `t`, or `p` rejects the whole string.
fn parse_params(s: &str) -> Option<(u32, u32, u32)> {
    let (mut m, mut t, mut p) = (None, None, None);
    for kv in s.split(',') {
        let (key, value) = kv.split_once('=')?;
        if key.is_empty() {
            return None;
        }
        let n: u32 = value.parse().ok()?;
        if n == 0 {
            return None;
        }
        match key {
            "m" => m = Some(n),
            "t" => t = Some(n),
            "p" => p = Some(n),
            _ => {}
        }
    }
    Some((m?, t?, p?))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[test]
    fn a_hash_verifies_against_the_right_password_only() {
        let stored = hash_admin_password("correct horse battery").unwrap();
        assert!(verify_admin_password("correct horse battery", &stored));
        assert!(!verify_admin_password("wrong horse battery", &stored));
        assert!(!verify_admin_password("", &stored));
    }

    #[test]
    fn the_written_format_is_the_one_the_typescript_reads() {
        let stored = hash_admin_password("pw").unwrap();
        let parts: Vec<&str> = stored.split('$').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "argon2id");
        assert_eq!(parts[1], "m=19456,t=2,p=1");
        // Padded standard base64, as the stored format uses.
        assert_eq!(STANDARD.decode(parts[2]).unwrap().len(), SALT_LEN);
        assert_eq!(STANDARD.decode(parts[3]).unwrap().len(), HASH_LEN);
        assert!(!stored.starts_with('$'), "PHC would; this format does not");
        assert!(!stored.contains("v="), "no version field, by format");
    }

    #[test]
    fn every_hash_gets_a_fresh_salt() {
        let a = hash_admin_password("same-password").unwrap();
        let b = hash_admin_password("same-password").unwrap();
        assert_ne!(a, b);
        assert!(verify_admin_password("same-password", &a));
        assert!(verify_admin_password("same-password", &b));
    }

    /// Parameters come from the stored string, so a hash written under a
    /// different profile still opens if the profile is ever raised.
    #[test]
    fn parameters_are_read_from_the_stored_string() {
        let salt = [0x11u8; SALT_LEN];
        let hash = derive("pw", &salt, 8, 1, 1).unwrap();
        let stored = format!(
            "argon2id$m=8,t=1,p=1${}${}",
            STANDARD.encode(salt),
            STANDARD.encode(hash.as_slice())
        );
        assert!(verify_admin_password("pw", &stored));
        assert!(!verify_admin_password("nope", &stored));
    }

    #[test]
    fn a_malformed_stored_string_returns_false_and_never_panics() {
        let good = hash_admin_password("pw").unwrap();
        let parts: Vec<&str> = good.split('$').collect();
        let salt = parts[2];
        let hash = parts[3];
        for bad in [
            "",
            "not-a-hash",
            "argon2id$m=19456,t=2,p=1",
            "argon2id$m=19456,t=2,p=1$$",
            &format!("argon2i$m=19456,t=2,p=1${salt}${hash}"),
            &format!("argon2id$m=0,t=2,p=1${salt}${hash}"),
            &format!("argon2id$m=abc,t=2,p=1${salt}${hash}"),
            &format!("argon2id$m=19456,p=1${salt}${hash}"),
            &format!("argon2id$m19456,t=2,p=1${salt}${hash}"),
            &format!("argon2id$=19456,t=2,p=1${salt}${hash}"),
            &format!("argon2id$m=19456,t=2,p=1$not!base64${hash}"),
            &format!("argon2id$m=19456,t=2,p=1${salt}$c2hvcnQ="),
            // Below argon2's 8-byte salt floor: false, not an error.
            &format!("argon2id$m=19456,t=2,p=1$AAAA${hash}"),
        ] {
            assert!(!verify_admin_password("pw", bad), "accepted {bad:?}");
        }
    }

    #[test]
    fn a_trailing_newline_in_the_file_is_tolerated() {
        let stored = hash_admin_password("pw").unwrap();
        assert!(verify_admin_password("pw", &format!("{stored}\n")));
        assert!(verify_admin_password("pw", &format!("  {stored}  ")));
    }

    #[test]
    fn unknown_parameter_keys_are_ignored() {
        let salt = [0x22u8; SALT_LEN];
        let hash = derive("pw", &salt, 8, 1, 1).unwrap();
        let stored = format!(
            "argon2id$m=8,t=1,p=1,keyid=7${}${}",
            STANDARD.encode(salt),
            STANDARD.encode(hash.as_slice())
        );
        assert!(verify_admin_password("pw", &stored));
    }

    #[derive(Deserialize)]
    struct PasswordFixture {
        vectors: Vec<PasswordVector>,
    }

    #[derive(Deserialize)]
    struct PasswordVector {
        note: String,
        password: String,
        stored: String,
    }

    /// The hashes sitting in `.admin-password` on every install that has set
    /// one. If this fails, upgrading locks the user out of remote pairing.
    #[test]
    fn typescript_hashes_still_verify() {
        let fixture: PasswordFixture =
            serde_json::from_str(include_str!("testdata/password-vectors.json")).unwrap();
        assert!(!fixture.vectors.is_empty());
        for v in &fixture.vectors {
            assert!(
                verify_admin_password(&v.password, &v.stored),
                "should verify: {}",
                v.note
            );
            assert!(
                !verify_admin_password(&format!("{}x", v.password), &v.stored),
                "should not verify with a changed password: {}",
                v.note
            );
        }
    }
}
