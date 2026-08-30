//! The signed seeded-extension manifest: shape, Ed25519 signature, cache, and
//! the rollback refusal.
//!
//! Three refusals: the signature is verified over `canonicalize(manifest minus
//! signature)` against the committed key; an id mismatch is refused (the bytes
//! would land under another extension's folder); a strictly OLDER manifest
//! than the cached one is refused (every past release was validly signed, so a
//! replay would roll back a fixed bug). The cache is re-verified on EVERY read:
//! it lives where anything running as the user can rewrite it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::compare_semver;
use crate::error::{AppError, ErrorCode, Result};
use crate::models::canonical::{canonicalize, verify_ed25519};

use crate::config::SIGNING_PUBLIC_KEY;

const SIG_ALGO_LABEL: &str = "ed25519-base64";

/// TODO(F2): no generated counterpart yet, so hand-written. The signature is
/// checked against the PARSED JSON, not a re-encode of this struct, so a field
/// this core does not model still survives into the signed message.
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

/// Parse a manifest document. `schemaVersion` must be exactly 1, checked by
/// hand because the struct types it `i64`; the deserializer refuses the rest.
pub fn parse_manifest(raw: &str) -> Result<SeededManifest> {
    let document: Value = serde_json::from_str(raw)
        .map_err(|err| fetch_failed(format!("invalid manifest JSON: {err}")))?;
    if document.get("schemaVersion") != Some(&Value::from(1)) {
        return Err(fetch_failed(format!(
            "unsupported schemaVersion: {}",
            document.get("schemaVersion").unwrap_or(&Value::Null)
        )));
    }
    serde_json::from_value(document)
        .map_err(|err| fetch_failed(format!("invalid manifest shape: {err}")))
}

/// Parse and verify one manifest document against the committed key.
pub fn verify_document(raw: &str) -> Result<SeededManifest> {
    verify_document_with(raw, SIGNING_PUBLIC_KEY)
}

/// The verification itself, with the key injected so tests drive this function
/// rather than a copy. The signed message is the parsed JSON minus `signature`.
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

/// Read a manifest the installer planted on disk and verify it OFFLINE. `None`
/// for absent, malformed, or unverifiable alike: boot's only decision is
/// seed-or-defer.
pub async fn read_planted(path: &Path) -> Option<SeededManifest> {
    let raw = tokio::fs::read_to_string(path).await.ok()?;
    verify_document(&raw).ok()
}

/// Where a verified manifest is cached, per extension id.
pub fn cache_path(cache_dir: &Path, extension_id: &str) -> PathBuf {
    cache_dir.join(format!("{extension_id}-manifest.json"))
}

/// The cached manifest, re-verified on every read.
pub async fn read_cached(cache_dir: &Path, extension_id: &str) -> Option<SeededManifest> {
    let raw = tokio::fs::read_to_string(cache_path(cache_dir, extension_id))
        .await
        .ok()?;
    verify_document(&raw).ok()
}

/// Write a verified manifest to the cache via a sibling temp file + rename.
pub async fn write_cached(cache_dir: &Path, extension_id: &str, raw: &str) -> Result<()> {
    tokio::fs::create_dir_all(cache_dir).await?;
    let path = cache_path(cache_dir, extension_id);
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, raw).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}

/// Decide between a freshly fetched manifest and the cached one: the manifest
/// to use, and whether the fetched one should be cached.
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
    /// False when the fetched manifest was refused, so it never reaches the cache.
    pub store: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use ed25519_dalek::{Signer, SigningKey};

    /// Signs with a throwaway key for [`verify_document_with`]; deliberately no
    /// second copy of the verification here.
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
        let cached = manifest("2.0.0", "builtin");
        let chosen = choose(manifest("1.9.9", "builtin"), Some(&cached), "builtin").unwrap();
        assert_eq!(chosen.manifest.version, "2.0.0");
        assert!(
            !chosen.store,
            "a refused older manifest was written to the cache, where it would win next boot"
        );

        // Equal is not older.
        let same = choose(manifest("2.0.0", "builtin"), Some(&cached), "builtin").unwrap();
        assert_eq!(same.manifest.version, "2.0.0");
        assert!(same.store);

        let newer = choose(manifest("2.0.1", "builtin"), Some(&cached), "builtin").unwrap();
        assert_eq!(newer.manifest.version, "2.0.1");
        assert!(newer.store);
    }

    #[test]
    fn with_no_cache_anything_signed_is_accepted() {
        let chosen = choose(manifest("0.0.1", "builtin"), None, "builtin").unwrap();
        assert_eq!(chosen.manifest.version, "0.0.1");
        assert!(chosen.store);
    }

    #[tokio::test]
    async fn a_planted_manifest_is_verified_and_a_tampered_one_is_simply_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".builtin.json");
        let (key, _) = keypair();

        tokio::fs::write(&path, signed_with(&key, document("1.0.0", "builtin")))
            .await
            .unwrap();
        assert!(
            read_planted(&path).await.is_none(),
            "an unverifiable plant was used"
        );

        tokio::fs::write(&path, "{ not json").await.unwrap();
        assert!(read_planted(&path).await.is_none());
        tokio::fs::remove_file(&path).await.unwrap();
        assert!(read_planted(&path).await.is_none());
    }

    #[tokio::test]
    async fn the_cache_is_re_verified_on_every_read_rather_than_trusted() {
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
