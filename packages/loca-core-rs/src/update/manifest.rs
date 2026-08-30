//! The signed core manifest: fetch, shape-check, verify, read.
//!
//! **The signature covers BYTES.** [`verify_document`] canonicalises the parsed
//! JSON *document* minus `signature`; the struct is only ever used to read
//! fields. Verifying a re-encoding of [`CoreManifest`] would drop any field
//! this core does not model, so the first field a future release adds would make
//! every deployed core report `signature_invalid` instead of tolerating drift.
//!
//! The envelope is checked and the entries are tolerated: one malformed entry is
//! skipped and the rest served, because refusing the document would make `deno`,
//! every worker and the updater uninstallable over one bad card.
//!
//! `workers` and `helpers` are optional here because every reader treats them
//! as optional; a required `Vec` would 422 a manifest that is otherwise fine.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{AppError, ErrorCode, Result};
use crate::models::canonical::{canonicalize, verify_ed25519};
use crate::update::net::Net;

use crate::config::SIGNING_PUBLIC_KEY;

/// The key every verification here uses. A test can point it at a throwaway
/// keypair, so the downgrade refusal and the whole check/apply path are driven
/// through the REAL entry points. Compiled out of production builds.
#[cfg(test)]
static TEST_PUBLIC_KEY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn trusted_public_key() -> String {
    #[cfg(test)]
    if let Some(key) = TEST_PUBLIC_KEY
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
    {
        return key;
    }
    SIGNING_PUBLIC_KEY.to_string()
}

/// A per-triple core executable.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct BinaryEntry {
    pub triple: String,
    pub url: String,
    pub sha256: String,
}

/// A platform-independent worker script, shipped separately from the compiled
/// binary and covered by the same signature because it is EXECUTED.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkerEntry {
    pub name: String,
    pub url: String,
    pub sha256: String,
}

/// A per-triple native helper. Only entries matching the running triple apply.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct HelperEntry {
    pub name: String,
    pub triple: String,
    pub url: String,
    pub sha256: String,
}

/// `CoreManifest` from `@loca/shared`. Hand-written because `domain/model.ts`
/// is not reachable from the contract generator yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreManifest {
    pub schema_version: u32,
    pub version: String,
    pub binaries: Vec<BinaryEntry>,
    /// `None` when the key is absent, `Some(vec![])` when present and empty:
    /// an older publisher versus one that shipped no workers.
    pub workers: Option<Vec<WorkerEntry>>,
    pub helpers: Option<Vec<HelperEntry>>,
    pub signature: String,
}

impl CoreManifest {
    /// The entry for `triple`, or `None`.
    pub fn binary_for(&self, triple: &str) -> Option<&BinaryEntry> {
        self.binaries.iter().find(|entry| entry.triple == triple)
    }

    /// Every worker, or an empty slice when the key was absent.
    pub fn workers(&self) -> &[WorkerEntry] {
        self.workers.as_deref().unwrap_or_default()
    }

    /// The helpers matching `triple`. Other platforms' are ignored, not errors.
    pub fn helpers_for(&self, triple: &str) -> Vec<&HelperEntry> {
        self.helpers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|helper| helper.triple == triple)
            .collect()
    }
}

fn invalid(what: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::ManifestFetchFailed, what.into())
}

/// Validate the envelope, then read the entries, so a garbled response cannot
/// reach the base64 decode with no signature to decode.
pub fn parse_manifest(value: &Value) -> Result<CoreManifest> {
    let Some(object) = value.as_object() else {
        return Err(invalid("core manifest is not an object"));
    };
    if object.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err(invalid("bad core manifest schemaVersion"));
    }
    let signature = non_empty_string(object.get("signature"))
        .ok_or_else(|| invalid("core manifest missing signature"))?;
    let version = non_empty_string(object.get("version"))
        .ok_or_else(|| invalid("core manifest missing version"))?;
    let Some(binaries) = object.get("binaries").and_then(Value::as_array) else {
        return Err(invalid("core manifest binaries is not an array"));
    };
    Ok(CoreManifest {
        schema_version: 1,
        version,
        binaries: entries(binaries, "binaries"),
        workers: optional_entries(object.get("workers"), "workers")?,
        helpers: optional_entries(object.get("helpers"), "helpers")?,
        signature,
    })
}

fn non_empty_string(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    (!text.is_empty()).then(|| text.to_string())
}

/// An absent key is `None`, a present non-array an error, and a present array
/// has its unreadable elements skipped.
fn optional_entries<T: for<'de> Deserialize<'de>>(
    value: Option<&Value>,
    key: &str,
) -> Result<Option<Vec<T>>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => Ok(Some(entries(items, key))),
        Some(_) => Err(invalid(format!("core manifest {key} is not an array"))),
    }
}

fn entries<T: for<'de> Deserialize<'de>>(items: &[Value], key: &str) -> Vec<T> {
    items
        .iter()
        .filter_map(|item| match serde_json::from_value::<T>(item.clone()) {
            Ok(entry) => Some(entry),
            Err(err) => {
                // Skipped rather than fatal, but logged so a malformed card
                // is diagnosable instead of just a shorter list.
                tracing::warn!(
                    target: "loca::update",
                    "skipping an unreadable {key} entry in the core manifest: {err}",
                );
                None
            }
        })
        .collect()
}

/// Parse `raw` and verify its Ed25519 signature over the canonical form of the
/// document **minus** `signature`.
///
/// The message is built from the parsed JSON, never from [`CoreManifest`].
pub fn verify_document(raw: &str, public_key_b64: &str) -> Result<CoreManifest> {
    let mut document: Value = serde_json::from_str(raw)
        .map_err(|err| invalid(format!("invalid core manifest JSON: {err}")))?;
    let manifest = parse_manifest(&document)?;
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
            "core manifest signature invalid",
        )
    })?;
    Ok(manifest)
}

/// Fetch and verify the signed core manifest. Reached only from a user action
/// or the install phase. Every failure below the fetch is
/// `manifest_fetch_failed`; only a real signature failure is
/// `signature_invalid`, so an unreachable manifest does not read as tampering.
pub async fn fetch_core_manifest(net: &Net, url: &str) -> Result<CoreManifest> {
    let raw = net
        .text(url)
        .await
        .map_err(|err| invalid(format!("core manifest fetch failed: {}", err.message)))?;
    verify_document(&raw, &trusted_public_key())
}

/// Signing and trust-root swapping for tests, public so the apply and route
/// suites can stage a manifest this module's PRODUCTION verifier accepts.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub mod test_trust {
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::Value;

    use super::{canonicalize, TEST_PUBLIC_KEY};

    static TRUST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Points [`super::trusted_public_key`] at a throwaway keypair for the
    /// life of the guard. Serialised, since the key is process-global. The
    /// claim turns a second `Trusted` in one test body into a named panic
    /// rather than a lock held across an await that wedges the suite.
    pub struct Trusted {
        signing: SigningKey,
        _guard: std::sync::MutexGuard<'static, ()>,
        _claim: crate::update::testing::FixtureClaim,
    }

    impl Default for Trusted {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Trusted {
        pub fn new() -> Self {
            // Claimed BEFORE the lock, so re-entry panics rather than parks.
            let claim = crate::update::testing::claim_fixture_lock("trust-root");
            let guard = TRUST_LOCK.lock().unwrap_or_else(|err| err.into_inner());
            let signing = SigningKey::from_bytes(&[13u8; 32]);
            let public = base64::engine::general_purpose::STANDARD
                .encode(signing.verifying_key().to_bytes());
            *TEST_PUBLIC_KEY
                .lock()
                .unwrap_or_else(|err| err.into_inner()) = Some(public);
            Self {
                signing,
                _guard: guard,
                _claim: claim,
            }
        }

        /// Sign `document` the way the release build does: Ed25519 over the
        /// canonical form of everything but `signature`, folded back in.
        pub fn sign(&self, mut document: Value) -> Value {
            if let Some(object) = document.as_object_mut() {
                object.remove("signature");
            }
            let signature = self.signing.sign(canonicalize(&document).as_bytes());
            let encoded = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
            if let Some(object) = document.as_object_mut() {
                object.insert("signature".into(), Value::String(encoded));
            }
            document
        }

        /// The signed document as the bytes an upstream would serve.
        pub fn sign_to_string(&self, document: Value) -> String {
            serde_json::to_string(&self.sign(document)).unwrap()
        }
    }

    impl Drop for Trusted {
        fn drop(&mut self) {
            *TEST_PUBLIC_KEY
                .lock()
                .unwrap_or_else(|err| err.into_inner()) = None;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::test_trust::Trusted;
    use super::*;
    use crate::update::testing::{bounded, published_manifest, Serve, Upstream};
    use serde_json::json;

    #[test]
    fn the_document_the_release_actually_publishes_parses() {
        // The fixture is transcribed from the published shape, not built from
        // the types above, which could not see those types being wrong.
        let manifest = parse_manifest(&published_manifest()).unwrap();
        assert_eq!(manifest.version, "0.2.0");
        assert_eq!(manifest.binaries.len(), 2);
        assert_eq!(manifest.workers().len(), 1);
        assert_eq!(
            manifest.binary_for("aarch64-apple-darwin").unwrap().sha256,
            "1111111111111111111111111111111111111111111111111111111111111111"
        );
        assert!(manifest.binary_for("mips-unknown-linux-gnu").is_none());
        // Only the entries for this triple apply; the rest are ignored.
        let helpers = manifest.helpers_for("x86_64-unknown-linux-gnu");
        assert_eq!(helpers.len(), 1);
        assert_eq!(helpers[0].name, "loca-core-updater");
    }

    #[test]
    fn a_field_this_core_does_not_model_still_verifies() {
        // The fixture carries a top-level key `CoreManifest` has no field for
        // and is signed over the WHOLE document. Verifying a re-serialisation
        // of the struct would drop it and read schema drift as forgery.
        let trusted = Trusted::new();
        let mut document = published_manifest();
        document.as_object_mut().unwrap().insert(
            "notaries".into(),
            json!([{ "url": "https://example.test" }]),
        );
        let raw = trusted.sign_to_string(document);

        let manifest = verify_document(&raw, &trusted_public_key()).unwrap();
        assert_eq!(manifest.version, "0.2.0");
    }

    #[test]
    fn a_tampered_document_is_refused() {
        let trusted = Trusted::new();
        let raw = trusted.sign_to_string(published_manifest());
        let tampered = raw.replace("0.2.0", "9.9.9");
        assert_ne!(tampered, raw, "the fixture must actually have changed");

        let err = verify_document(&tampered, &trusted_public_key()).unwrap_err();
        assert_eq!(err.code, ErrorCode::SignatureInvalid);
    }

    #[test]
    fn a_manifest_from_a_future_schema_is_refused() {
        let mut document = published_manifest();
        document.as_object_mut().unwrap()["schemaVersion"] = json!(2);
        let err = parse_manifest(&document).unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
        assert!(err.message.contains("schemaVersion"), "{}", err.message);
    }

    #[test]
    fn a_manifest_with_no_signature_is_refused_before_decoding() {
        // Both shapes of "no signature" are staged: an empty string and an
        // absent key take different branches.
        for document in [
            {
                let mut d = published_manifest();
                d.as_object_mut().unwrap().remove("signature");
                d
            },
            {
                let mut d = published_manifest();
                d.as_object_mut().unwrap()["signature"] = json!("");
                d
            },
        ] {
            let err = parse_manifest(&document).unwrap_err();
            assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
            assert!(err.message.contains("signature"), "{}", err.message);
        }
    }

    #[test]
    fn a_manifest_with_no_version_is_refused() {
        let mut document = published_manifest();
        document.as_object_mut().unwrap()["version"] = json!("");
        let err = parse_manifest(&document).unwrap_err();
        assert!(err.message.contains("version"), "{}", err.message);
    }

    #[test]
    fn a_non_array_binaries_is_refused_and_an_absent_workers_is_not() {
        let mut document = published_manifest();
        document.as_object_mut().unwrap()["binaries"] = json!({ "triple": "x" });
        let err = parse_manifest(&document).unwrap_err();
        assert!(err.message.contains("binaries"), "{}", err.message);

        // `binaries` must be an array; `workers` and `helpers` may be absent.
        let mut document = published_manifest();
        let object = document.as_object_mut().unwrap();
        object.remove("workers");
        object.remove("helpers");
        let manifest = parse_manifest(&document).unwrap();
        assert_eq!(manifest.workers, None);
        assert_eq!(manifest.helpers, None);
        assert!(manifest.workers().is_empty());
        assert!(manifest.helpers_for("aarch64-apple-darwin").is_empty());
    }

    #[test]
    fn an_absent_collection_stays_distinct_from_an_empty_one() {
        // Folded together, a release that ships no workers would be
        // indistinguishable from one published before workers existed.
        let mut document = published_manifest();
        document.as_object_mut().unwrap()["workers"] = json!([]);
        let manifest = parse_manifest(&document).unwrap();
        assert_eq!(manifest.workers, Some(Vec::new()));
    }

    #[test]
    fn a_non_array_workers_is_still_refused() {
        let mut document = published_manifest();
        document.as_object_mut().unwrap()["workers"] = json!("tool-worker.ts");
        let err = parse_manifest(&document).unwrap_err();
        assert!(err.message.contains("workers"), "{}", err.message);
    }

    #[test]
    fn one_unreadable_entry_does_not_take_the_document_down() {
        // Refusing the document over one bad card would make `deno`, every
        // worker and the updater uninstallable, with nothing pointing at it.
        let mut document = published_manifest();
        document.as_object_mut().unwrap()["binaries"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "triple": "riscv64-unknown-linux-gnu" }));
        let manifest = parse_manifest(&document).unwrap();
        assert_eq!(manifest.binaries.len(), 2, "the good entries survive");
        assert!(manifest.binary_for("riscv64-unknown-linux-gnu").is_none());
    }

    #[tokio::test]
    async fn an_unreachable_manifest_reads_as_unreachable_rather_than_tampered() {
        // A 404 mid-rollout and a forged document must not read the same.
        let up = Upstream::start(Serve::Status(404)).await;
        let net = Net::new().unwrap();
        let err = bounded(fetch_core_manifest(&net, &up.url("/manifests/core.json")))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
    }

    #[tokio::test]
    async fn a_fetched_signed_manifest_verifies_end_to_end() {
        let trusted = Trusted::new();
        let raw = trusted.sign_to_string(published_manifest());
        let up = Upstream::start_routes(vec![(
            "/manifests/core.json",
            Serve::Body(raw.into_bytes()),
        )])
        .await;
        let net = Net::new().unwrap();
        let manifest = bounded(fetch_core_manifest(&net, &up.url("/manifests/core.json")))
            .await
            .unwrap();
        assert_eq!(manifest.version, "0.2.0");
        assert_eq!(up.calls(), vec!["/manifests/core.json"]);
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_a_fetch_failure_rather_than_a_panic() {
        let up = Upstream::start(Serve::Body(b"<!doctype html>".to_vec())).await;
        let net = Net::new().unwrap();
        let err = bounded(fetch_core_manifest(&net, &up.url("/manifests/core.json")))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
    }
}
