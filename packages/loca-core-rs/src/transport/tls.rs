//! TLS material for the HTTPS/WSS API.
//!
//! Core serves the API over TLS with a SELF-SIGNED certificate; clients pin it,
//! because no public CA issues for `127.0.0.1` or a LAN address. The trust
//! model:
//!
//! - An ECDSA P-256 keypair is generated once and sealed in the secrets vault
//!   as a private JWK. ECDSA P-256 rather than Ed25519 because rustls and
//!   browsers support it and Ed25519 TLS certificates are poorly supported.
//! - The certificate is regenerated on EVERY boot from that key with the
//!   current SANs. Cheap, and it keeps the SANs honest as the bind host moves.
//! - The pin is `base64(SHA-256(SubjectPublicKeyInfo))`. It depends only on the
//!   KEY, so it survives certificate regeneration and bind-host changes and no
//!   client has to re-pair when an address changes. This matches the RFC 7469
//!   and rustls pin conventions, and it is what the Tauri client computes from
//!   the presented certificate.
//!
//! **The pin derivation is byte-exact.** Every already-paired client pinned this
//! value, so a derivation that drifts by one byte breaks all of them on upgrade
//! and presents as "pairing stopped working". `testdata/tls_pin_vectors.json`
//! is the guard.
//!
//! No vault I/O here: [`ensure`] takes the sealed JWK and hands back the one to
//! seal, which keeps the byte-exact surface a pure function.

use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use p256::SecretKey;
use rand::RngCore;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType, PKCS_ECDSA_P256_SHA256,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

use crate::error::{internal, validation, Result};

/// The vault entry the sealed private JWK lives under. The frontend reads and
/// writes it; this module never touches the vault itself.
pub const KEY_SECRET: &str = "tls-key";

/// Certificate lifetime. The certificate is regenerated every boot anyway, so
/// this only has to outlive one run.
const CERT_VALID_YEARS: i64 = 10;
/// Backdate tolerance for a client whose clock runs slightly behind ours.
const CLOCK_SKEW_SECONDS: i64 = 60;

/// Everything the listener and the pairing handshake need from TLS.
#[derive(Clone, Debug)]
pub struct TlsMaterial {
    pub cert_der: CertificateDer<'static>,
    /// The private key as PKCS#8 DER. Bytes rather than a `PrivateKeyDer`,
    /// which rustls makes non-`Clone` on purpose; [`TlsMaterial::key_der`] is
    /// the explicit copy.
    key_pkcs8: Vec<u8>,
    /// The public certificate in PEM, written beside the core root so a
    /// co-located admin tool can trust the loopback API as a CA of one.
    pub cert_pem: String,
    /// `base64(SHA-256(SPKI DER))`: the value clients pin and the pairing
    /// handshake binds.
    pub pin: String,
}

impl TlsMaterial {
    /// A fresh key handle for a rustls `ServerConfig`.
    pub fn key_der(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_pkcs8.clone()))
    }
}

/// Build the per-boot TLS material.
///
/// `sealed` is the private JWK already in the vault, or `None` on first run.
/// The second element of the result is `Some(jwk)` exactly when a key was
/// generated and the caller must seal it; an existing key is never rewritten,
/// because rewriting it would re-pin every paired client.
///
/// ```ignore
/// let sealed = vault.get(tls::KEY_SECRET).await?;
/// let (tls, new_jwk) = tls::ensure(sealed.as_deref(), &bind_host)?;
/// if let Some(jwk) = new_jwk { vault.set(tls::KEY_SECRET, &jwk).await?; }
/// ```
pub fn ensure(sealed: Option<&str>, bind_host: &str) -> Result<(TlsMaterial, Option<String>)> {
    match sealed {
        Some(jwk) => {
            let key = secret_key_from_jwk(jwk)?;
            Ok((build(&key, bind_host)?, None))
        }
        None => {
            let key = generate_key()?;
            let material = build(&key, bind_host)?;
            tracing::info!(
                target: "loca::tls",
                "generated new TLS keypair (ECDSA P-256), sealing in the secrets vault",
            );
            Ok((material, Some(jwk_from_secret_key(&key)?)))
        }
    }
}

/// `base64(SHA-256(SPKI DER))` for a P-256 public key: export as SPKI, hash
/// with SHA-256, then standard base64 (padded, not URL-safe). The SPKI is 91
/// bytes: a fixed 26-byte header, `0x04`, then the two 32-byte affine
/// coordinates each zero-padded to full width.
fn pin_for(key: &SecretKey) -> Result<String> {
    let spki = key
        .public_key()
        .to_public_key_der()
        .map_err(|e| internal(format!("could not encode SPKI: {e}")))?;
    Ok(B64.encode(Sha256::digest(spki.as_bytes())))
}

fn build(key: &SecretKey, bind_host: &str) -> Result<TlsMaterial> {
    let pkcs8 = key
        .to_pkcs8_der()
        .map_err(|e| internal(format!("could not encode PKCS#8: {e}")))?;
    let key_pair = KeyPair::try_from(pkcs8.as_bytes())
        .map_err(|e| internal(format!("could not load the TLS keypair: {e}")))?;

    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, "loca-core");
    params.serial_number = Some(random_serial().into());
    let now = OffsetDateTime::now_utc();
    params.not_before = now - Duration::seconds(CLOCK_SKEW_SECONDS);
    params.not_after = now + Duration::days(CERT_VALID_YEARS * 365);
    // `ExplicitNoCa` emits basicConstraints as critical with CA:FALSE, which
    // the variant name does not say.
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.subject_alt_names = sans_for(bind_host)?;

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| internal(format!("could not build the TLS certificate: {e}")))?;

    Ok(TlsMaterial {
        cert_der: cert.der().clone(),
        key_pkcs8: pkcs8.as_bytes().to_vec(),
        cert_pem: cert.pem(),
        pin: pin_for(key)?,
    })
}

fn random_serial() -> Vec<u8> {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.to_vec()
}

/// SANs for the certificate: loopback always, plus the bind host when it is a
/// concrete address.
///
/// `0.0.0.0` means "every interface" rather than a connectable name, so it is
/// skipped. A pinning verifier ignores SANs entirely, so this is cosmetic for
/// the shipping client and correct for anything doing standard validation.
fn sans_for(bind_host: &str) -> Result<Vec<SanType>> {
    let mut sans = vec![
        SanType::DnsName(
            "localhost"
                .try_into()
                .map_err(|_| internal("localhost is not a valid DNS SAN"))?,
        ),
        SanType::IpAddress(
            "127.0.0.1"
                .parse()
                .map_err(|_| internal("bad loopback v4"))?,
        ),
        SanType::IpAddress("::1".parse().map_err(|_| internal("bad loopback v6"))?),
    ];
    let extra = bind_host.trim();
    if extra.is_empty() || extra == "0.0.0.0" || is_already_covered(extra) {
        return Ok(sans);
    }
    match extra.parse() {
        Ok(ip) => sans.push(SanType::IpAddress(ip)),
        Err(_) => sans.push(SanType::DnsName(extra.try_into().map_err(|_| {
            validation(format!("bind host is not a usable SAN: {extra}"))
        })?)),
    }
    Ok(sans)
}

fn is_already_covered(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

// --- JWK ------------------------------------------------------------------

/// The private JWK shape WebCrypto's `exportKey("jwk")` produces for an ECDSA
/// P-256 private key. The declaration order below IS the wire order, measured
/// rather than assumed, so the vault plaintext stays byte-identical.
///
/// Unknown fields are ignored on the way in: a stricter parser would turn a
/// harmless extra field into a lost keypair, which costs every paired client.
#[derive(Debug, Serialize, Deserialize)]
struct PrivateJwk {
    kty: String,
    crv: String,
    /// Defaulted, because refusing a vault entry that predates the field would
    /// discard the keypair and re-pin every paired client. `ES256` is the only
    /// algorithm a P-256 signing key here can carry.
    #[serde(default = "es256")]
    alg: String,
    x: String,
    y: String,
    d: String,
    #[serde(default = "sign_ops")]
    key_ops: Vec<String>,
    #[serde(default = "yes")]
    ext: bool,
}

fn yes() -> bool {
    true
}

fn es256() -> String {
    "ES256".to_string()
}

fn sign_ops() -> Vec<String> {
    vec!["sign".to_string()]
}

fn secret_key_from_jwk(raw: &str) -> Result<SecretKey> {
    let jwk: PrivateJwk = serde_json::from_str(raw)
        .map_err(|e| validation(format!("sealed TLS key is not a JWK: {e}")))?;
    if jwk.kty != "EC" || jwk.crv != "P-256" {
        return Err(validation(format!(
            "sealed TLS key is {}/{}, expected EC/P-256",
            jwk.kty, jwk.crv
        )));
    }
    let d = coordinate(&jwk.d, "d")?;
    let key = SecretKey::from_slice(&d)
        .map_err(|e| validation(format!("sealed TLS key has an invalid scalar: {e}")))?;

    // The public coordinates are derivable from `d`, so they are checked
    // rather than trusted: a mismatch means the vault entry is corrupt.
    let point = key.public_key().to_encoded_point(false);
    let (x, y) = (coordinate(&jwk.x, "x")?, coordinate(&jwk.y, "y")?);
    if point.x().map(|v| v.as_slice() != x).unwrap_or(true)
        || point.y().map(|v| v.as_slice() != y).unwrap_or(true)
    {
        return Err(validation(
            "sealed TLS key is inconsistent: the public coordinates do not match the scalar",
        ));
    }
    Ok(key)
}

fn jwk_from_secret_key(key: &SecretKey) -> Result<String> {
    let point = key.public_key().to_encoded_point(false);
    let x = point
        .x()
        .ok_or_else(|| internal("public point has no x coordinate"))?;
    let y = point
        .y()
        .ok_or_else(|| internal("public point has no y coordinate"))?;
    let jwk = PrivateJwk {
        kty: "EC".to_string(),
        crv: "P-256".to_string(),
        alg: es256(),
        x: B64URL.encode(x),
        y: B64URL.encode(y),
        d: B64URL.encode(key.to_bytes()),
        key_ops: sign_ops(),
        ext: true,
    };
    // serde writes fields in declaration order, chosen to match WebCrypto.
    Ok(serde_json::to_string(&jwk)?)
}

/// Decode one base64url field to its full 32-byte width. The width check is the
/// point: a coordinate with a leading zero byte shortens under any big-integer
/// encoder, giving a plausible, stable, WRONG pin.
fn coordinate(raw: &str, name: &str) -> Result<[u8; 32]> {
    let bytes = B64URL
        .decode(raw.as_bytes())
        .map_err(|e| validation(format!("TLS key field `{name}` is not base64url: {e}")))?;
    bytes.try_into().map_err(|v: Vec<u8>| {
        validation(format!(
            "TLS key field `{name}` is {} bytes, expected 32",
            v.len()
        ))
    })
}

/// Generate a fresh P-256 key. Routed through rcgen rather than
/// `SecretKey::random` so the crate does not pull `p256`'s `rand_core` 0.6.
fn generate_key() -> Result<SecretKey> {
    let pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|e| internal(format!("could not generate a TLS keypair: {e}")))?;
    SecretKey::from_pkcs8_der(&pair.serialize_der())
        .map_err(|e| internal(format!("could not import the generated key: {e}")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct VectorFile {
        vectors: Vec<Vector>,
    }

    #[derive(Deserialize)]
    struct Vector {
        note: String,
        jwk: serde_json::Value,
        pin: String,
    }

    fn vectors() -> Vec<Vector> {
        let raw = include_str!("testdata/tls_pin_vectors.json");
        serde_json::from_str::<VectorFile>(raw).unwrap().vectors
    }

    /// The pin test. If it fails, do not update the fixture: a change here
    /// means every already-paired client is about to stop trusting this core.
    #[test]
    fn pins_match_the_webcrypto_vectors() {
        for v in vectors() {
            let jwk = serde_json::to_string(&v.jwk).unwrap();
            let (material, resealed) = ensure(Some(&jwk), "127.0.0.1").unwrap();
            assert_eq!(material.pin, v.pin, "pin drifted for: {}", v.note);
            assert!(resealed.is_none(), "an existing key must never be resealed");
        }
    }

    /// A second encoder agreeing with `p256` narrows a future pin mismatch down
    /// to the hash or the base64 rather than leaving DER a suspect. rcgen only
    /// exposes its SPKI as PEM, so the armour comes off here.
    #[test]
    fn rcgen_and_p256_agree_on_the_spki_bytes() {
        for v in vectors() {
            let key = secret_key_from_jwk(&serde_json::to_string(&v.jwk).unwrap()).unwrap();
            let from_p256 = key.public_key().to_public_key_der().unwrap();
            let pair = KeyPair::try_from(key.to_pkcs8_der().unwrap().as_bytes()).unwrap();
            let from_rcgen = B64
                .decode(
                    pair.public_key_pem()
                        .lines()
                        .filter(|l| !l.starts_with("-----"))
                        .collect::<String>(),
                )
                .unwrap();
            assert_eq!(
                from_p256.as_bytes(),
                from_rcgen.as_slice(),
                "SPKI encoders disagree for: {}",
                v.note
            );
        }
    }

    /// The shape the pin depends on, asserted directly: a 91-byte SPKI whose
    /// last 64 bytes are the two full-width coordinates. A coordinate that lost
    /// a leading zero would shorten this and change the pin.
    #[test]
    fn spki_is_a_fixed_header_plus_two_full_width_coordinates() {
        const HEADER: &[u8] = &[
            0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06,
            0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00, 0x04,
        ];
        for v in vectors() {
            let key = secret_key_from_jwk(&serde_json::to_string(&v.jwk).unwrap()).unwrap();
            let der = key.public_key().to_public_key_der().unwrap();
            let bytes = der.as_bytes();
            assert_eq!(bytes.len(), 91, "SPKI length changed for: {}", v.note);
            assert_eq!(&bytes[..HEADER.len()], HEADER, "SPKI header changed");
            let point = key.public_key().to_encoded_point(false);
            assert_eq!(&bytes[27..59], point.x().unwrap().as_slice());
            assert_eq!(&bytes[59..91], point.y().unwrap().as_slice());
        }
    }

    /// The pin depends on the KEY and nothing else, which is what lets a core
    /// change address, regenerate its certificate, and reboot without any
    /// paired client having to re-pair.
    #[test]
    fn the_pin_survives_rebuilds_bind_hosts_and_reboots() {
        let jwk = serde_json::to_string(&vectors()[0].jwk).unwrap();
        let (a, _) = ensure(Some(&jwk), "127.0.0.1").unwrap();
        let (b, _) = ensure(Some(&jwk), "192.168.1.50").unwrap();
        let (c, _) = ensure(Some(&jwk), "core.example.test").unwrap();
        assert_eq!(a.pin, b.pin);
        assert_eq!(a.pin, c.pin);
        // Different certificates (random serial, different SANs), same pin.
        assert_ne!(a.cert_der, b.cert_der);
    }

    #[test]
    fn a_generated_key_round_trips_through_the_webcrypto_jwk_shape() {
        let (material, sealed) = ensure(None, "127.0.0.1").unwrap();
        let jwk = sealed.expect("a first run must hand back a key to seal");

        // Read off the RAW string: `serde_json`'s map is a `BTreeMap` here, so
        // a parsed `Value` re-sorts the keys and cannot measure field order.
        let keys: Vec<&str> = jwk
            .split(",\"")
            .map(|part| part.trim_start_matches('{').trim_start_matches('"'))
            .filter_map(|part| part.split("\":").next())
            .collect();
        assert_eq!(
            keys,
            ["kty", "crv", "alg", "x", "y", "d", "key_ops", "ext"],
            "the vault plaintext must match WebCrypto's exportKey(\"jwk\") field \
             for field AND in order"
        );
        let parsed: serde_json::Value = serde_json::from_str(&jwk).unwrap();
        let obj = parsed.as_object().unwrap();
        assert_eq!(obj["alg"], "ES256");
        assert_eq!(obj["kty"], "EC");
        assert_eq!(obj["crv"], "P-256");
        assert_eq!(obj["ext"], true);
        assert_eq!(obj["key_ops"], serde_json::json!(["sign"]));
        for field in ["x", "y", "d"] {
            let v = obj[field].as_str().unwrap();
            assert!(!v.contains('='), "{field} must be unpadded base64url");
            assert!(
                !v.contains('+') && !v.contains('/'),
                "{field} must be url-safe"
            );
            assert_eq!(B64URL.decode(v).unwrap().len(), 32);
        }

        // Reloading reproduces the same pin: the boot path of a second run.
        let (again, resealed) = ensure(Some(&jwk), "127.0.0.1").unwrap();
        assert_eq!(again.pin, material.pin);
        assert!(resealed.is_none());
    }

    #[test]
    fn a_corrupt_sealed_key_fails_loudly() {
        assert!(ensure(Some("not json"), "127.0.0.1").is_err());
        assert!(ensure(
            Some(r#"{"kty":"OKP","crv":"Ed25519","x":"","y":"","d":""}"#),
            "h"
        )
        .is_err());

        // A scalar one byte short, as a big-integer encoder produces: it must
        // be rejected, not silently left-padded.
        let mut jwk: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&vectors()[0].jwk).unwrap()).unwrap();
        let d = jwk["d"].as_str().unwrap().to_string();
        let mut bytes = B64URL.decode(&d).unwrap();
        bytes.remove(0);
        jwk["d"] = serde_json::Value::String(B64URL.encode(&bytes));
        let err = ensure(Some(&jwk.to_string()), "127.0.0.1").unwrap_err();
        assert!(err.message.contains("31 bytes"), "{}", err.message);

        // Public coordinates that do not belong to the scalar.
        let mut jwk: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&vectors()[0].jwk).unwrap()).unwrap();
        jwk["x"] = jwk["y"].clone();
        assert!(ensure(Some(&jwk.to_string()), "127.0.0.1").is_err());
    }

    #[test]
    fn sans_cover_loopback_and_the_bind_host_without_duplicating_it() {
        let loopback = sans_for("127.0.0.1").unwrap();
        assert_eq!(loopback.len(), 3, "an already-covered host is not re-added");
        assert_eq!(sans_for("localhost").unwrap().len(), 3);
        assert_eq!(sans_for("::1").unwrap().len(), 3);
        // "all interfaces" is not a connectable name.
        assert_eq!(sans_for("0.0.0.0").unwrap().len(), 3);
        assert_eq!(sans_for("  ").unwrap().len(), 3);

        let lan = sans_for("192.168.1.50").unwrap();
        assert_eq!(lan.len(), 4);
        assert!(matches!(lan[3], SanType::IpAddress(_)));

        let dns = sans_for("core.example.test").unwrap();
        assert_eq!(dns.len(), 4);
        assert!(matches!(dns[3], SanType::DnsName(_)));
    }

    #[test]
    fn the_certificate_is_a_usable_server_leaf() {
        let (material, _) = ensure(None, "192.168.1.50").unwrap();
        assert!(material.cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(!material.cert_der.is_empty());
        // rustls only accepts a key it can parse and match to the certificate.
        // Through `server::provider()`, not `ServerConfig::builder()`: two
        // providers are linked in the test build, so the bare builder panics
        // unless an earlier test installed a process default first.
        assert!(
            rustls::ServerConfig::builder_with_provider(super::super::server::provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![material.cert_der.clone()], material.key_der())
                .is_ok()
        );
    }
}
