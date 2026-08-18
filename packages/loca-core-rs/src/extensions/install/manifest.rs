//! The signed seeded-extension manifest: shape, Ed25519 signature, cache, and
//! the rollback refusal.
//!
//! Ports `packages/loca-core/src/extensions/seeded-manifest.ts`.
//!
//! A "seeded" extension is one Loca ships: the built-in, which is distributed
//! from the CDN and never from npm, and the dev-only samples showcase. This
//! module answers one question, "which version, and which tarball bytes", and
//! the answer is only ever trusted because it is signed.
//!
//! # Three refusals, and why each is a control rather than tidiness
//!
//! - **The signature is verified over `canonicalize(manifest minus signature)`**,
//!   against the key committed into the binary. Nothing here trusts a manifest
//!   it has not verified, including one it wrote to its own cache.
//! - **An id mismatch is refused.** The production endpoint is the built-in's,
//!   and installing its bytes under another extension's folder would plant code
//!   the user never asked for under a name they did.
//! - **A strictly OLDER manifest than the cached one is refused.** Every past
//!   release was validly signed, so without this a replayed old manifest rolls
//!   the extension back to a version whose bug was fixed. `version` is inside
//!   the signature, which is what makes the comparison meaningful.
//!
//! # The cache is a cache, not a trust anchor
//!
//! It is re-verified on EVERY read, not only when it was written. A cache file
//! is on disk where anything running as the user can edit it, so a cache that
//! is trusted because it was once verified is a signature check with a
//! writable bypass.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::compare_semver;
use crate::error::{AppError, ErrorCode, Result};
use crate::models::canonical::{canonicalize, verify_ed25519};

use crate::config::SIGNING_PUBLIC_KEY;

const SIG_ALGO_LABEL: &str = "ed25519-base64";

/// TODO(F2): `BuiltinExtensionManifest` (`domain/model.ts:409`) has no
/// generated counterpart, so this is an interim hand-written shape under
/// D-031's precedent. It is the whole document: a field this core does not
/// model would be dropped by a re-encode, which is why the signature is checked
/// against the PARSED JSON rather than against this struct (see
/// [`verify_document`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeededManifest {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i64,
    pub version: String,
    pub id: String,
    #[serde(rename = "tarballUrl")]
    pub tarball_url: String,
    pub sha256: String,
    pub signature: String,
}

fn fetch_failed(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::ManifestFetchFailed, message)
}

/// Parse a manifest document.
///
/// `schemaVersion` must be exactly 1. A core that met a version 2 document and
/// read the fields it recognised would be guessing about the ones it did not,
/// and one of those could be what makes the signature mean something different.
/// That check is the only hand-written one here; the rest is the deserializer's.
pub fn parse_manifest(raw: &str) -> Result<SeededManifest> {
    let document: Value = serde_json::from_str(raw)
        .map_err(|err| fetch_failed(format!("invalid manifest JSON: {err}")))?;
    // `schemaVersion` is checked BY HAND because the struct types it `i64`, so
    // deserializing accepts a version 2 document without comment. Every other
    // field is a required `String` there, so `from_value` refuses a missing or
    // mistyped one first: the specification's per-field loop exists because
    // TypeScript has no deserializer, and carrying it across would be a guard
    // that cannot fire.
    if document.get("schemaVersion") != Some(&Value::from(1)) {
        return Err(fetch_failed(format!(
            "unsupported schemaVersion: {}",
            document.get("schemaVersion").unwrap_or(&Value::Null)
        )));
    }
    serde_json::from_value(document)
        .map_err(|err| fetch_failed(format!("invalid manifest shape: {err}")))
}

/// Parse and verify one manifest document.
///
/// The signed message is built from the PARSED JSON with `signature` removed,
/// never by re-encoding [`SeededManifest`]. Re-encoding would silently drop any
/// field this core does not model, so a schema addition would read as a forged
/// signature on every already-deployed core.
pub fn verify_document(raw: &str) -> Result<SeededManifest> {
    verify_document_with(raw, SIGNING_PUBLIC_KEY)
}

/// The verification itself, with the trusted key as a parameter.
///
/// The key is injected rather than read from the constant so that TESTS DRIVE
/// THIS FUNCTION rather than a copy of it. That is not a stylistic preference:
/// an earlier version of this module kept a key-injected duplicate in its test
/// module, and a mutation replacing the signed message with a re-encode of the
/// parsed struct survived the whole suite, because the only test that could
/// have seen it was looking at the duplicate. Every other manifest verifier in
/// this crate already takes its key this way.
pub fn verify_document_with(raw: &str, public_key_b64: &str) -> Result<SeededManifest> {
    let manifest = parse_manifest(raw)?;
    let mut document: Value = serde_json::from_str(raw)
        .map_err(|err| fetch_failed(format!("invalid manifest JSON: {err}")))?;
    if let Some(object) = document.as_object_mut() {
        object.remove("signature");
    }
    verify_ed25519(
        public_key_b64,
        &manifest.signature,
        canonicalize(&document).as_bytes(),
    )
    .map_err(|_| {
        AppError::new(
            ErrorCode::SignatureInvalid,
            format!("seeded extension manifest signature ({SIG_ALGO_LABEL}) verification failed"),
        )
    })?;
    Ok(manifest)
}

/// Read a manifest the installer planted on disk and verify it OFFLINE.
///
/// `None` for absent, malformed, or unverifiable, with no distinction between
/// them: this is boot, the caller's only decision is "seed or defer to the
/// user", and a missing plant and a tampered plant lead to the same place.
///
/// **Nothing in this function can reach the network**, which is the property
/// the whole first-boot path rests on.
pub async fn read_planted(path: &Path) -> Option<SeededManifest> {
    let raw = tokio::fs::read_to_string(path).await.ok()?;
    verify_document(&raw).ok()
}

/// Where a verified manifest is cached, per extension id.
pub fn cache_path(cache_dir: &Path, extension_id: &str) -> PathBuf {
    cache_dir.join(format!("{extension_id}-manifest.json"))
}

/// The cached manifest, re-verified.
///
/// Re-verified on every read rather than trusted because it was verified once:
/// the file lives where anything running as the user can rewrite it.
pub async fn read_cached(cache_dir: &Path, extension_id: &str) -> Option<SeededManifest> {
    let raw = tokio::fs::read_to_string(cache_path(cache_dir, extension_id))
        .await
        .ok()?;
    verify_document(&raw).ok()
}

/// Write a verified manifest to the cache, via a sibling temp file.
///
/// Renamed rather than written in place, so a crash mid-write cannot leave a
/// truncated document that the next read would reject and re-fetch forever.
pub async fn write_cached(cache_dir: &Path, extension_id: &str, raw: &str) -> Result<()> {
    tokio::fs::create_dir_all(cache_dir).await?;
    let path = cache_path(cache_dir, extension_id);
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, raw).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}

/// Decide between a freshly fetched manifest and the cached one.
///
/// Split out from any I/O so the rollback rule is a pure function over two
/// versions. Returns the manifest to USE, and whether the fetched one should be
/// cached.
pub fn choose(
    fetched: SeededManifest,
    cached: Option<&SeededManifest>,
    expected_id: &str,
) -> Result<Chosen> {
    if fetched.id != expected_id {
        return Err(fetch_failed(format!(
            "seeded manifest id mismatch: requested {expected_id}, fetched {}",
            fetched.id
        )));
    }
    if let Some(cached) = cached {
        if compare_semver(&fetched.version, &cached.version) == std::cmp::Ordering::Less {
            tracing::warn!(
                target: "loca::extensions",
                "refusing older seeded extension manifest v{}; keeping cached v{}",
                fetched.version, cached.version,
            );
            return Ok(Chosen {
                manifest: cached.clone(),
                store: false,
            });
        }
    }
    Ok(Chosen {
        manifest: fetched,
        store: true,
    })
}

#[derive(Debug, Clone)]
pub struct Chosen {
    pub manifest: SeededManifest,
    /// False when the fetched manifest was refused, so a rolled-back document
    /// never reaches the cache and cannot win on the next boot by being there.
    pub store: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use ed25519_dalek::{Signer, SigningKey};

    /// A manifest signed with the REAL committed key cannot be produced here,
    /// so the tests that need a valid signature sign with a throwaway key and
    /// pass it to [`verify_document_with`], which IS the production function
    /// with its key as a parameter. There is deliberately no second copy of the
    /// verification in this module: a copy is a reimplementation, and a mutation
    /// of the real one sails past every test that only drives the copy.
    fn signed_with(key: &SigningKey, mut document: serde_json::Map<String, Value>) -> String {
        document.remove("signature");
        let message = canonicalize(&Value::Object(document.clone()));
        let signature = key.sign(message.as_bytes());
        document.insert(
            "signature".to_string(),
            Value::String(base64_encode(&signature.to_bytes())),
        );
        serde_json::to_string(&Value::Object(document)).unwrap()
    }

    fn base64_encode(bytes: &[u8]) -> String {
        use base64::prelude::{Engine as _, BASE64_STANDARD};
        BASE64_STANDARD.encode(bytes)
    }

    fn document(version: &str, id: &str) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "schemaVersion": 1,
            "version": version,
            "id": id,
            "tarballUrl": "https://get.loca.rzkyif.com/x.tgz",
            "sha256": "a".repeat(64),
            "signature": "",
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn manifest(version: &str, id: &str) -> SeededManifest {
        SeededManifest {
            schema_version: 1,
            version: version.to_string(),
            id: id.to_string(),
            tarball_url: "https://get.loca.rzkyif.com/x.tgz".to_string(),
            sha256: "a".repeat(64),
            signature: String::new(),
        }
    }

    fn keypair() -> (SigningKey, String) {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        (key.clone(), base64_encode(key.verifying_key().as_bytes()))
    }

    #[test]
    fn a_manifest_signed_by_the_wrong_key_is_refused() {
        // Through the PRODUCTION function, against the real committed key. A
        // throwaway keypair is by construction not it.
        let (key, _) = keypair();
        let raw = signed_with(&key, document("1.0.0", "loca-extension-builtin"));
        let err = verify_document(&raw).unwrap_err();
        assert_eq!(err.code, ErrorCode::SignatureInvalid);
    }

    #[test]
    fn a_valid_signature_verifies_and_a_tampered_field_does_not() {
        let (key, public) = keypair();
        let raw = signed_with(&key, document("1.2.3", "loca-extension-builtin"));
        assert_eq!(
            verify_document_with(&raw, &public).unwrap().version,
            "1.2.3"
        );

        // Every field is inside the signature. Moving any one of them has to
        // break it, or the signature is decorative: `sha256` is what anchors
        // the tarball, `tarballUrl` is where the bytes come from, `version` is
        // what the rollback rule compares, and `id` is which folder they land
        // in.
        for field in ["version", "id", "tarballUrl", "sha256"] {
            let mut document: Value = serde_json::from_str(&raw).unwrap();
            document[field] = Value::String("tampered".to_string());
            let tampered = serde_json::to_string(&document).unwrap();
            assert!(
                verify_document_with(&tampered, &public).is_err(),
                "{field} is outside the signature"
            );
        }
    }

    #[test]
    fn the_signed_message_is_the_document_and_not_the_struct() {
        // A field this core does not model must survive into the signed
        // message. Re-encoding the parsed struct would drop it, and every
        // already-deployed core would then read a schema addition as a forged
        // signature: the manifest verifies for the publisher and fails for
        // every user at once.
        let (key, public) = keypair();
        let mut document = document("1.0.0", "loca-extension-builtin");
        document.insert("futureField".to_string(), Value::String("kept".to_string()));
        let raw = signed_with(&key, document);
        assert!(verify_document_with(&raw, &public).is_ok());
    }

    #[test]
    fn a_document_that_is_not_a_version_one_manifest_is_refused_before_any_signature_work() {
        for (raw, why) in [
            ("not json", "unparseable"),
            ("[]", "not an object"),
            ("{\"schemaVersion\":1}", "missing every field"),
            (
                "{\"schemaVersion\":1,\"version\":\"1\",\"id\":\"x\",\"tarballUrl\":\"u\",\"sha256\":1,\"signature\":\"s\"}",
                "sha256 is not a string",
            ),
        ] {
            let err = parse_manifest(raw).unwrap_err();
            assert_eq!(err.code, ErrorCode::ManifestFetchFailed, "{why}");
        }
    }

    #[test]
    fn a_complete_document_of_another_schema_version_is_still_refused() {
        // The schemaVersion pin is the only hand-written screen left, and it is
        // observable ONLY against a document that is otherwise valid: fed one
        // that is also missing every other field, the deserializer refuses it
        // whatever the version check does, and the pin could be deleted with
        // nothing turning red. It survived a mutation for exactly that reason.
        let mut doc = document("1.0.0", "loca-extension-builtin");
        assert!(
            parse_manifest(&serde_json::to_string(&Value::Object(doc.clone())).unwrap()).is_ok(),
            "the fixture is not otherwise-valid, so this would prove nothing"
        );
        for version in [
            Value::from(0),
            Value::from(2),
            Value::String("1".to_string()),
            Value::Null,
        ] {
            doc.insert("schemaVersion".to_string(), version.clone());
            let raw = serde_json::to_string(&Value::Object(doc.clone())).unwrap();
            let err = parse_manifest(&raw).unwrap_err();
            assert_eq!(err.code, ErrorCode::ManifestFetchFailed, "{version}");
            assert!(err.message.contains("schemaVersion"), "{}", err.message);
        }
    }

    #[test]
    fn a_manifest_for_another_extension_is_refused() {
        // The production endpoint is the built-in's. Accepting a mismatched id
        // would plant its bytes under a folder named for something else.
        let err = choose(
            manifest("1.0.0", "loca-extension-samples"),
            None,
            "loca-extension-builtin",
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
        assert!(err.message.contains("mismatch"), "{}", err.message);
    }

    #[test]
    fn a_strictly_older_manifest_is_refused_and_never_cached() {
        // Every past release was validly signed, so a replay is a legitimate
        // document; the version comparison is the only thing standing between
        // a replayed old manifest and a rollback to a version whose bug was
        // fixed.
        let cached = manifest("2.0.0", "builtin");
        let chosen = choose(manifest("1.9.9", "builtin"), Some(&cached), "builtin").unwrap();
        assert_eq!(chosen.manifest.version, "2.0.0");
        assert!(
            !chosen.store,
            "a refused older manifest was written to the cache, where it would win next boot"
        );

        // Equal is not older: a re-fetch of the SAME version must still be
        // usable, or a cache wipe leaves the extension permanently unresolvable.
        let same = choose(manifest("2.0.0", "builtin"), Some(&cached), "builtin").unwrap();
        assert_eq!(same.manifest.version, "2.0.0");
        assert!(same.store);

        let newer = choose(manifest("2.0.1", "builtin"), Some(&cached), "builtin").unwrap();
        assert_eq!(newer.manifest.version, "2.0.1");
        assert!(newer.store);
    }

    #[test]
    fn with_no_cache_anything_signed_is_accepted() {
        // There is nothing to roll back from on a first install.
        let chosen = choose(manifest("0.0.1", "builtin"), None, "builtin").unwrap();
        assert_eq!(chosen.manifest.version, "0.0.1");
        assert!(chosen.store);
    }

    #[tokio::test]
    async fn a_planted_manifest_is_verified_and_a_tampered_one_is_simply_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".builtin.json");
        let (key, _) = keypair();

        // Signed by a throwaway key, so it does NOT verify against the
        // committed one: the boot path sees "no usable plant" and defers.
        tokio::fs::write(&path, signed_with(&key, document("1.0.0", "builtin")))
            .await
            .unwrap();
        assert!(
            read_planted(&path).await.is_none(),
            "an unverifiable plant was used"
        );

        // Absent, malformed and unverifiable are deliberately the same answer:
        // boot's only decision is seed-or-defer.
        tokio::fs::write(&path, "{ not json").await.unwrap();
        assert!(read_planted(&path).await.is_none());
        tokio::fs::remove_file(&path).await.unwrap();
        assert!(read_planted(&path).await.is_none());
    }

    #[tokio::test]
    async fn the_cache_is_re_verified_on_every_read_rather_than_trusted() {
        // The file is on disk where anything running as the user can rewrite
        // it. A cache trusted because it was verified once is a signature check
        // with a writable bypass.
        let dir = tempfile::tempdir().unwrap();
        let (key, _) = keypair();
        let raw = signed_with(&key, document("1.0.0", "builtin"));
        write_cached(dir.path(), "builtin", &raw).await.unwrap();
        assert!(cache_path(dir.path(), "builtin").exists());
        assert!(
            read_cached(dir.path(), "builtin").await.is_none(),
            "a manifest signed by the wrong key was served from the cache"
        );
    }

    #[tokio::test]
    async fn the_cache_write_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        write_cached(dir.path(), "builtin", "{}").await.unwrap();
        let mut names = Vec::new();
        let mut entries = tokio::fs::read_dir(dir.path()).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        assert_eq!(names, vec!["builtin-manifest.json".to_string()]);
    }
}
