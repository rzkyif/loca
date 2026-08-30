//! Canonical JSON, JavaScript's number formatting, and the Ed25519 check.
//!
//! The signed catalog's signature covers `canonicalize(payload)`, and
//! `canonicalize` is defined in terms of `JSON.stringify`, which makes
//! JavaScript's rendering of a scalar part of the wire format. One rule differs
//! from the obvious Rust one: `serde_json::to_string(&4.0f64)` is `"4.0"` where
//! `JSON.stringify(4.0)` is `"4"`, and every integral value in the catalog goes
//! through that path, so a naive re-serialisation fails EVERY signature.
//!
//! The number rendering here is therefore ECMAScript's `Number::toString`,
//! pinned by `testdata/canonical-vectors.json`. Written with no dependency on
//! anything else in this module, because the binary and core manifests verify
//! the same way against the same key.

use std::cmp::Ordering;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::Value;

use crate::error::{AppError, ErrorCode, Result};

/// Recursively sorted keys, arrays in order, every scalar rendered the way
/// `JSON.stringify` renders it.
pub fn canonicalize(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        // Always through `f64`: JavaScript has one number type, so emitting an
        // `i64`/`u64` verbatim would be MORE precise than the reference
        // implementation and therefore wrong.
        Value::Number(n) => out.push_str(&js_number(n.as_f64().unwrap_or(f64::NAN))),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| utf16_cmp(a, b));
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, key);
                out.push(':');
                if let Some(v) = map.get(key) {
                    write_value(out, v);
                }
            }
            out.push('}');
        }
    }
}

/// `Object.keys(o).sort()` orders by UTF-16 **code unit**, and Rust's `str`
/// ordering is by UTF-8 byte. The two disagree for supplementary characters: a
/// surrogate pair's lead unit is in `0xD800..0xE000`, so an emoji sorts BELOW
/// `U+FFFD` in JavaScript and ABOVE it in Rust.
///
/// No key in the catalog is non-ASCII, where the two orders coincide. This
/// exists because the canonical form is a signature input and "no key is
/// non-ASCII today" is not a property anything enforces.
fn utf16_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `serde_json`'s string escaping is `JSON.stringify`'s: the seven short
/// escapes, `\u00XX` for the remaining control characters, and no escaping of
/// `/` or of anything non-ASCII.
fn write_string(out: &mut String, s: &str) {
    match serde_json::to_string(s) {
        Ok(rendered) => out.push_str(&rendered),
        // Serialising a `&str` cannot fail. Rendering nothing would silently
        // corrupt a signature input, so this is a visible wrong answer instead.
        Err(_) => out.push_str("\"<unserialisable>\""),
    }
}

/// ECMAScript's `Number::toString` for a double, which is what
/// `JSON.stringify` emits for a finite number.
///
/// The spec's shape: take the shortest decimal `digits` and the position `n` of
/// the decimal point such that the value is `0.digits * 10^n`, then choose
/// between plain and exponential notation by where `n` falls. Rust's `LowerExp`
/// supplies that shortest form, so this is a reformat rather than a
/// float-to-decimal implementation. The thresholds are the whole point:
/// JavaScript switches to exponential at `1e21` and below `1e-6`, and Rust's
/// `Display` never switches at all.
pub fn js_number(value: f64) -> String {
    // Covers -0.0, which `JSON.stringify` renders as "0".
    if value == 0.0 {
        return "0".to_string();
    }
    if !value.is_finite() {
        // `JSON.stringify(NaN)` and `(Infinity)` are both "null".
        return "null".to_string();
    }
    let negative = value < 0.0;
    let exp_form = format!("{:e}", value.abs());
    let Some((mantissa, exponent)) = exp_form.split_once('e') else {
        return exp_form;
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return exp_form;
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    // `{:e}` gives `d.ddd * 10^exponent`; the spec's `n` is the decimal point's
    // position in `0.ddd * 10^n`, one further left.
    let n = exponent + 1;

    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if k <= n && n <= 21 {
        // 1500000000
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        // 16.3
        let split = n as usize;
        out.push_str(&digits[..split]);
        out.push('.');
        out.push_str(&digits[split..]);
    } else if -6 < n && n <= 0 {
        // 0.000001
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        // 1e+21, 1e-7, 1.7976931348623157e+308
        if k == 1 {
            out.push_str(&digits);
        } else {
            out.push_str(&digits[..1]);
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        let e = n - 1;
        if e >= 0 {
            out.push('+');
        }
        out.push_str(&e.to_string());
    }
    out
}

/// Decode a base64 Ed25519 public key into a verifying key.
///
/// Split out of [`verify_ed25519`] so the baked-in trust root can be checked for
/// usability without a signature to check it against.
pub fn decode_verifying_key(public_key_b64: &str) -> Result<VerifyingKey> {
    let invalid = |what: &str| AppError::new(ErrorCode::SignatureInvalid, what.to_string());

    let key_bytes = B64
        .decode(public_key_b64.trim())
        .map_err(|_| invalid("signing key is not valid base64"))?;
    let key: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| invalid("signing key is not 32 bytes"))?;
    VerifyingKey::from_bytes(&key).map_err(|_| invalid("signing key is not on the curve"))
}

/// Verify a base64 Ed25519 signature over `message` against a base64 public key.
/// The trust root for `catalog.json`, `binaries.json` and `core.json`.
pub fn verify_ed25519(public_key_b64: &str, signature_b64: &str, message: &[u8]) -> Result<()> {
    let invalid = |what: &str| AppError::new(ErrorCode::SignatureInvalid, what.to_string());

    let key = decode_verifying_key(public_key_b64)?;

    let sig_bytes = B64
        .decode(signature_b64.trim())
        .map_err(|_| invalid("signature is not valid base64"))?;
    let sig: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| invalid("signature is not 64 bytes"))?;

    key.verify(message, &Signature::from_bytes(&sig))
        .map_err(|_| invalid("signature verification failed"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// Generated by `deno task contract` from the real `canonicalize` and the
    /// real catalog. See the module docs.
    const VECTORS: &str = include_str!("testdata/canonical-vectors.json");
    const DEV_CATALOG: &str = include_str!("catalog.dev.json");

    fn vectors() -> Value {
        serde_json::from_str(VECTORS).expect("the fixture parses")
    }

    #[test]
    fn every_number_renders_the_way_javascript_renders_it() {
        let v = vectors();
        let cases = v["numbers"].as_array().expect("numbers is an array");
        assert!(cases.len() >= 20, "the table should not shrink silently");
        for case in cases {
            let input = case["input"].as_str().expect("input is a string");
            let expected = case["output"].as_str().expect("output is a string");
            let parsed: f64 = input.parse().expect("input parses as a double");
            assert_eq!(
                js_number(parsed),
                expected,
                "JSON.stringify({input}) is {expected}"
            );
        }
    }

    /// The rule that makes the whole module necessary, asserted on its own so a
    /// failure names it rather than pointing at a 37 KB digest.
    #[test]
    fn an_integral_double_loses_its_fraction() {
        assert_eq!(js_number(4.0), "4");
        assert_eq!(serde_json::to_string(&4.0f64).unwrap(), "4.0");
    }

    #[test]
    fn the_sample_shapes_canonicalise_exactly() {
        let v = vectors();
        let samples = v["samples"].as_array().expect("samples is an array");
        assert!(!samples.is_empty());
        for sample in samples {
            let label = sample["label"].as_str().unwrap_or("?");
            let expected = sample["canonical"].as_str().expect("canonical is a string");
            assert_eq!(canonicalize(&sample["value"]), expected, "sample: {label}");
        }
    }

    /// Canonicalise the real dev payload and verify a real Ed25519 signature
    /// over its own canonical form: the digest assertion says where the two
    /// disagree, the signature assertion says it matters.
    #[test]
    fn the_dev_catalog_canonicalises_to_the_recorded_signed_bytes() {
        let embedded: Value = serde_json::from_str(DEV_CATALOG).expect("the dev catalog parses");
        let canonical = canonicalize(&embedded["payload"]);

        let v = vectors();
        let expected_len = v["catalogCanonicalBytes"].as_u64().expect("a length");
        assert_eq!(
            canonical.len() as u64,
            expected_len,
            "canonical byte length"
        );

        let digest = Sha256::digest(canonical.as_bytes());
        assert_eq!(
            hex::encode(digest),
            v["catalogCanonicalSha256"].as_str().expect("a digest"),
            "canonical form of the dev catalog"
        );

        verify_ed25519(
            v["publicKey"].as_str().expect("a key"),
            v["signature"].as_str().expect("a signature"),
            canonical.as_bytes(),
        )
        .expect("the recorded signature verifies against our canonical form");
    }

    #[test]
    fn a_tampered_message_or_signature_is_rejected() {
        let v = vectors();
        let embedded: Value = serde_json::from_str(DEV_CATALOG).expect("the dev catalog parses");
        let canonical = canonicalize(&embedded["payload"]);
        let key = v["publicKey"].as_str().expect("a key");
        let sig = v["signature"].as_str().expect("a signature");

        let mut tampered = canonical.clone();
        tampered.push(' ');
        assert!(verify_ed25519(key, sig, tampered.as_bytes()).is_err());

        // A different key of the right shape, so the failure is the signature
        // rather than the decode.
        let other = B64.encode([0u8; 32]);
        assert!(verify_ed25519(&other, sig, canonical.as_bytes()).is_err());

        assert!(verify_ed25519(key, "not base64!!", canonical.as_bytes()).is_err());
        assert!(verify_ed25519(key, &B64.encode([0u8; 10]), canonical.as_bytes()).is_err());
    }

    /// `serde_json::Map` iterates in UTF-8 byte order, which is the opposite
    /// answer for a supplementary character. Sorting by code unit is what makes
    /// this pass.
    #[test]
    fn keys_sort_by_utf16_code_unit() {
        let value: Value = serde_json::from_str(r#"{"�":1,"😀":2}"#).unwrap();
        assert_eq!(canonicalize(&value), "{\"\u{1f600}\":2,\"\u{fffd}\":1}");
    }

    #[test]
    fn the_exponent_thresholds_are_where_javascript_puts_them() {
        // Plain right up to the boundary, exponential at it.
        assert_eq!(js_number(1e20), "100000000000000000000");
        assert_eq!(js_number(1e21), "1e+21");
        assert_eq!(js_number(1e-6), "0.000001");
        assert_eq!(js_number(1e-7), "1e-7");
        // Rust's Display would render this with 324 decimal places.
        assert_eq!(js_number(5e-324), "5e-324");
        assert_eq!(js_number(-0.0), "0");
    }
}
