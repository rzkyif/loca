//! `binaries.json`: fetch it, verify its Ed25519 signature, cache it, and refuse
//! a replay.
//!
//! # Two published shapes, and both ship
//!
//! An entry is one of two things, decided by whether it carries a `resolver`
//! key (`domain/model.ts:227`, `isResolverEntry`):
//!
//! - **Pinned**, `{ version, platforms: { <triple>: <target-or-variant-map> } }`.
//!   What the STABLE channel publishes: every URL and sha256 resolved at release
//!   time, so the signed manifest commits to the exact bytes and installing
//!   needs no third party.
//! - **Resolver**, `{ resolver: { repo, assets: { <triple>: <pattern-or-map> },
//!   pinnedTag? } }`. What the LATEST channel publishes, and what a dev core
//!   builds in-code: the signed manifest commits to the repo and the patterns,
//!   and the concrete release is resolved at runtime against GitHub's published
//!   sha256 digest.
//!
//! Both channels ALSO carry the self-hosted `loca-core-speech` entry in the
//! pinned shape on every channel (`scripts/release/core.ts:875`), so "latest
//! means resolver" is not true of a whole document, only of an entry.
//!
//! # An entry this core cannot read is not a manifest this core cannot read
//!
//! [`BinaryManifestEntry::Unknown`] catches anything that is neither shape. The
//! oracle validates the document's envelope and never looks inside `binaries`
//! (`manifest.ts:106-127`), so a future kind with a third shape leaves it
//! working and would have failed every route here. An unknown entry resolves to
//! "not available for this host", which is the same answer the oracle gives for
//! a triple it does not cover.
//!
//! # The cache is a performance optimisation, never a trust anchor
//!
//! The signature is re-verified on **every read of the cache**, not only on
//! fetch. A cache that is trusted because it is ours is a file any local process
//! can rewrite, so verifying only on the way in would make the on-disk copy a
//! second, weaker trust root. Verifying on the way out means a tampered cache is
//! indistinguishable from no cache, which is the correct outcome.
//!
//! # The signature covers the BYTES, not this struct
//!
//! Verification canonicalises the parsed JSON document minus `signature`, never
//! a re-serialisation of [`BinaryManifest`]. D6 explains why at
//! `models/catalog.rs:111-117` and it is sharper here: this struct deliberately
//! does not model everything a manifest may carry, so re-encoding it would drop
//! fields out of the message and every deployed core would report a schema
//! addition as `SignatureInvalid`, which reads as an attack rather than as
//! drift.
//!
//! # The replay refusal
//!
//! A validly-signed OLDER manifest is still validly signed. Serving one would
//! roll binaries back to a release with a fixed vulnerability, so a forced
//! refresh that returns a strictly-older `version` keeps the cached manifest
//! instead. The signature covers `version`, which is what makes the comparison
//! meaningful rather than something an attacker can set freely. Same-version is
//! ACCEPTED (the comparison is `< 0`, matching `manifest.ts:54`): a re-publish
//! at the same version is a normal republish, not a rollback.
//!
//! # Dev builds no manifest and fetches nothing
//!
//! A dev core runs from source, so there is no published `manifests/dev/
//! binaries.json` to fetch or verify. The resolver config compiled into the
//! build IS the trust anchor there, so the manifest is built in-code, offline.
//! That is also what keeps the consent rule intact for a dev boot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{compare_semver, Channel};
use crate::downloads::binaries::upstream::{TripleAsset, UpstreamResolver};
use crate::downloads::cancel::CancellationToken;
use crate::downloads::net::Net;
use crate::error::{AppError, ErrorCode, Result};
use crate::models::canonical::{canonicalize, verify_ed25519};

use crate::config::SIGNING_PUBLIC_KEY;

const SIG_ALGO_LABEL: &str = "ed25519-base64";

/// The key every verification here uses.
///
/// A test can point it at a throwaway keypair, which is what lets the replay
/// refusal, the cache short-circuit and the tampered-cache refusal be driven
/// through the REAL [`load_manifest`] rather than through a copy of it that
/// takes a key parameter. That copy is how the replay refusal came to have no
/// test at all: the test asserted the comparison the branch is built on and
/// never called the function containing the branch, so deleting the branch left
/// the suite green. Compiled out of every production build, so nothing a caller
/// supplies can reach it.
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

/// The variant every core installs. GPU builds exist upstream and are selected
/// by the Deno core from detected hardware; this core installs one kind
/// (`deno`), which publishes one variant.
pub const CPU_VARIANT: &str = "cpu";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct BinaryManifest {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub version: String,
    /// Keyed by binary kind. A `BTreeMap` rather than a `HashMap` because the
    /// canonical form this is verified against is key-sorted.
    pub binaries: BTreeMap<String, BinaryManifestEntry>,
    /// Base64 Ed25519 over the canonical form of everything above.
    #[serde(default)]
    pub signature: String,
}

/// One kind's entry. See the module docs: the two published shapes plus a
/// catch-all that keeps one unreadable entry from failing the document.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum BinaryManifestEntry {
    /// `latest`, and a dev core's in-code manifest.
    Resolver {
        resolver: UpstreamResolver,
    },
    /// `stable`, and the self-hosted kinds on every channel.
    Pinned {
        version: String,
        /// Keyed by triple.
        platforms: BTreeMap<String, VariantPlatform>,
    },
    Unknown(Value),
}

/// One resolved, pinned download: the primary plus any companion archives.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PinnedTarget {
    pub url: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extras: Option<Vec<PinnedExtra>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PinnedExtra {
    pub url: String,
    pub sha256: String,
}

/// A triple's pinned targets: a bare one (single-variant, read as `cpu`) or a
/// per-variant map. Mirror of [`TripleAsset`] for the pinned shape.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum VariantPlatform {
    One(PinnedTarget),
    Variants(BTreeMap<String, PinnedTarget>),
}

impl VariantPlatform {
    /// The target for `variant`, normalising both shapes the way
    /// `platformVariants` (`domain/model.ts:152-156`) does.
    pub fn variant(&self, variant: &str) -> Option<&PinnedTarget> {
        match self {
            Self::One(target) => (variant == CPU_VARIANT).then_some(target),
            Self::Variants(map) => map.get(variant),
        }
    }
}

impl BinaryManifestEntry {
    /// The version this entry reports to `GET /api/v1/binaries` WITHOUT
    /// resolving anything upstream.
    ///
    /// A pinned entry knows its version; a resolver entry does not until it
    /// asks GitHub, so it reports the installed one or the literal `latest`,
    /// which is `manager.ts:293-297`. Reaching out to answer a plain list is
    /// what the consent rule forbids.
    pub fn version_without_network(&self, installed: Option<&str>) -> String {
        match self {
            Self::Pinned { version, .. } => version.clone(),
            Self::Resolver { .. } => installed.unwrap_or("latest").to_string(),
            Self::Unknown(_) => installed.unwrap_or("unknown").to_string(),
        }
    }

    /// The asset config this entry offers for `triple`, when it is a resolver.
    pub fn resolver_asset(&self, triple: &str) -> Option<&TripleAsset> {
        match self {
            Self::Resolver { resolver } => resolver.assets.get(triple),
            _ => None,
        }
    }
}

/// Where the verified manifest is cached between boots.
pub fn cache_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("binaries-manifest.json")
}

/// The manifest this core should resolve binaries from.
///
/// `force` re-fetches even when a good cache exists; without it a valid cache
/// short-circuits and no request is made, which is what lets a boot install a
/// missing binary offline.
pub async fn load_manifest(
    net: &Net,
    channel: Channel,
    cache_dir: &Path,
    manifest_url: &str,
    force: bool,
    cancel: &CancellationToken,
) -> Result<BinaryManifest> {
    if channel == Channel::Dev {
        return Ok(dev_manifest());
    }
    let cached = cached_manifest(cache_dir).await;
    if let Some(cached) = cached.clone() {
        if !force {
            return Ok(cached);
        }
    }
    let (fetched, raw) = fetch_and_verify(net, manifest_url, cancel).await?;
    if let Some(cached) = cached {
        // Strictly older only. See the module docs on why same-version passes.
        if compare_semver(&fetched.version, &cached.version) == std::cmp::Ordering::Less {
            tracing::warn!(
                target: "loca::downloads",
                "refusing older binary manifest v{}; keeping cached v{}",
                fetched.version,
                cached.version
            );
            return Ok(cached);
        }
    }
    write_cached(cache_dir, &raw).await?;
    Ok(fetched)
}

/// The cached manifest, or `None` for anything that is not a good one.
///
/// Every failure collapses to `None`: missing, unreadable, malformed, or
/// **failing signature verification**. See the module docs: a tampered cache has
/// to be indistinguishable from no cache.
///
/// This is also the whole of what `GET /api/v1/binaries` may consult, which is
/// why it is public: a "cached, or nothing" read cannot be expressed as
/// `load_manifest(force: false)`, because that falls through to a fetch on every
/// one of the four ways a cache read returns `None`.
pub async fn cached_manifest(cache_dir: &Path) -> Option<BinaryManifest> {
    let text = tokio::fs::read_to_string(cache_path(cache_dir))
        .await
        .ok()?;
    verify_document(&text, &trusted_public_key()).ok()
}

/// The in-code manifest a dev core resolves from. No network, no signature: the
/// config compiled into this build is the trust anchor.
pub fn dev_manifest() -> BinaryManifest {
    let mut assets = BTreeMap::new();
    assets.insert(
        crate::config::host_triple().to_string(),
        TripleAsset::One(crate::downloads::binaries::upstream::VariantAsset::Name(
            format!("deno-{}.zip", crate::config::host_triple()),
        )),
    );
    let mut binaries = BTreeMap::new();
    binaries.insert(
        "deno".to_string(),
        BinaryManifestEntry::Resolver {
            resolver: UpstreamResolver {
                repo: "denoland/deno".into(),
                assets,
                pinned_tag: Some(crate::sidecars::deno::PINNED_DENO_TAG.to_string()),
            },
        },
    );
    BinaryManifest {
        schema_version: 1,
        version: crate::config::CORE_VERSION.to_string(),
        binaries,
        signature: String::new(),
    }
}

/// Fetch, verify, and return both the parsed manifest and the exact text that
/// was verified, so the cache stores the bytes rather than a re-encoding.
async fn fetch_and_verify(
    net: &Net,
    manifest_url: &str,
    cancel: &CancellationToken,
) -> Result<(BinaryManifest, String)> {
    let raw = net.text(manifest_url, cancel).await?;
    let manifest = verify_document(&raw, &trusted_public_key())?;
    Ok((manifest, raw))
}

/// Shape-check before anything trusts a field.
pub fn parse_manifest(value: &Value) -> Result<BinaryManifest> {
    let invalid = |what: String| AppError::new(ErrorCode::ManifestFetchFailed, what);
    if !value.is_object() {
        return Err(invalid("manifest is not an object".into()));
    }
    let manifest: BinaryManifest = serde_json::from_value(value.clone())
        .map_err(|err| invalid(format!("invalid binary manifest: {err}")))?;
    if manifest.schema_version != 1 {
        return Err(invalid(format!(
            "unsupported schemaVersion: {}",
            manifest.schema_version
        )));
    }
    if manifest.version.is_empty() {
        return Err(invalid("manifest missing version".into()));
    }
    if manifest.signature.is_empty() {
        return Err(invalid("manifest missing signature".into()));
    }
    Ok(manifest)
}

/// Parse `raw` and verify its Ed25519 signature over the canonical form of the
/// document **minus** `signature`.
///
/// The message is built from the parsed JSON, never from [`BinaryManifest`]. See
/// the module docs: re-encoding the struct would silently drop any field this
/// core does not model, and a schema addition would then read as a forged
/// signature on every deployed core.
pub fn verify_document(raw: &str, public_key_b64: &str) -> Result<BinaryManifest> {
    let invalid = |what: String| AppError::new(ErrorCode::ManifestFetchFailed, what);
    let mut document: Value = serde_json::from_str(raw)
        .map_err(|err| invalid(format!("invalid manifest JSON: {err}")))?;
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
            format!("manifest signature ({SIG_ALGO_LABEL}) verification failed"),
        )
    })?;
    Ok(manifest)
}

async fn write_cached(cache_dir: &Path, raw: &str) -> Result<()> {
    tokio::fs::create_dir_all(cache_dir).await?;
    let path = cache_path(cache_dir);
    let tmp = path.with_extension("json.tmp");
    // Written to a sibling and renamed, so a crash mid-write cannot leave a
    // truncated file that the next boot reads as a tampered cache.
    tokio::fs::write(&tmp, raw).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}

/// Signing and trust-root swapping for tests, here rather than in `mod tests`
/// because the installer's tests need to stage a cache this module's PRODUCTION
/// verifier accepts. A test that stages one the production path would reject
/// proves nothing about the production path.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub mod test_trust {
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::Value;

    use super::{canonicalize, TEST_PUBLIC_KEY};

    static TRUST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Points [`super::trusted_public_key`] at a throwaway keypair for the life
    /// of the guard. Serialised, because the key is process-global and cargo
    /// runs tests in threads.
    pub struct Trusted {
        signing: SigningKey,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Default for Trusted {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Trusted {
        pub fn new() -> Self {
            let guard = TRUST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let signing = SigningKey::from_bytes(&[7u8; 32]);
            let public = base64::engine::general_purpose::STANDARD
                .encode(signing.verifying_key().to_bytes());
            *TEST_PUBLIC_KEY.lock().unwrap_or_else(|e| e.into_inner()) = Some(public);
            Self {
                signing,
                _guard: guard,
            }
        }

        /// Sign `document` the way the release build does: Ed25519 over the
        /// canonical form of everything except `signature`.
        pub fn sign(&self, document: Value) -> String {
            sign_with(&self.signing, document)
        }
    }

    impl Drop for Trusted {
        fn drop(&mut self) {
            *TEST_PUBLIC_KEY.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    /// Sign with an arbitrary key, for the "someone else signed this" cases.
    pub fn sign_with(signing: &SigningKey, mut document: Value) -> String {
        if let Some(object) = document.as_object_mut() {
            object.remove("signature");
        }
        let sig = signing.sign(canonicalize(&document).as_bytes());
        document["signature"] =
            Value::String(base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()));
        serde_json::to_string(&document).unwrap()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use base64::Engine;
    use ed25519_dalek::SigningKey;

    use super::test_trust::Trusted;
    use super::*;
    use crate::downloads::testing::{
        bounded, published_latest_manifest, published_stable_manifest, Serve, Upstream,
    };

    /// A throwaway keypair, so the tests exercise the real verification path
    /// rather than a stub, without needing the production private key.
    fn keypair() -> (SigningKey, String) {
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let public =
            base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().to_bytes());
        (signing, public)
    }

    /// Sign a document the way the release build does.
    fn signed(document: Value, signing: &SigningKey) -> String {
        test_trust::sign_with(signing, document)
    }

    /// The stable document at a chosen manifest version.
    fn stable_at(version: &str) -> Value {
        let mut document = published_stable_manifest();
        document["version"] = Value::String(version.to_string());
        document
    }

    // -- the two shipping shapes --------------------------------------------

    #[test]
    fn the_documents_the_release_actually_publishes_parse() {
        // The review's worst finding: every earlier test hand-built the shape
        // the parser already expected, so none of them could see that neither
        // SHIPPING channel publishes it. Stable publishes `{ version,
        // platforms }` with no `resolver` key at all, and latest publishes
        // `assets` as a per-triple MAP.
        for (channel, document) in [
            ("stable", published_stable_manifest()),
            ("latest", published_latest_manifest()),
        ] {
            let mut document = document;
            document["signature"] = serde_json::json!("not-checked-here");
            let manifest = parse_manifest(&document)
                .unwrap_or_else(|err| panic!("{channel} binaries.json did not parse: {err:?}"));
            assert!(
                manifest.binaries.contains_key("deno"),
                "{channel}: deno is the kind this core installs"
            );
        }
    }

    #[test]
    fn a_stable_entry_is_pinned_and_a_latest_entry_resolves() {
        let mut stable = published_stable_manifest();
        stable["signature"] = serde_json::json!("x");
        let stable = parse_manifest(&stable).unwrap();
        let host = crate::config::host_triple();

        let BinaryManifestEntry::Pinned { version, platforms } = &stable.binaries["deno"] else {
            panic!("a stable entry carries no resolver, so it must read as pinned");
        };
        assert_eq!(version, "v2.8.2", "the pinned version is the release TAG");
        let target = platforms[host].variant(CPU_VARIANT).unwrap();
        assert!(
            target.url.ends_with(&format!("deno-{host}.zip")),
            "{target:?}"
        );
        assert_eq!(target.sha256.len(), 64);

        let mut latest = published_latest_manifest();
        latest["signature"] = serde_json::json!("x");
        let latest = parse_manifest(&latest).unwrap();
        let BinaryManifestEntry::Resolver { resolver } = &latest.binaries["deno"] else {
            panic!("a latest entry carries a resolver");
        };
        assert_eq!(resolver.repo, "denoland/deno");
        assert_eq!(resolver.pinned_tag.as_deref(), Some("v2.8.2"));
        assert_eq!(
            resolver.assets[host]
                .variant(CPU_VARIANT)
                .map(|a| a.asset()),
            Some(format!("deno-{host}.zip").as_str())
        );
    }

    #[test]
    fn the_gpu_variant_matrix_survives_both_shapes() {
        // `llama-server` is the entry that exercises every corner of the schema:
        // a bare string for a single-variant triple, a per-variant map
        // otherwise, and `{ asset, extra }` / `extras` for the companion
        // archives a cuda build needs. This core installs none of it, and it
        // still has to PARSE, because one unreadable entry must not fail the
        // document that also carries `deno`.
        let mut latest = published_latest_manifest();
        latest["signature"] = serde_json::json!("x");
        let latest = parse_manifest(&latest).unwrap();
        let assets = latest.binaries["llama-server"]
            .resolver_asset("x86_64-pc-windows-msvc")
            .unwrap();
        let cuda = assets.variant("cuda").unwrap();
        assert_eq!(cuda.asset(), "llama-{tag}-bin-win-cuda-13.3-x64.zip");
        assert_eq!(cuda.extra(), ["cudart-llama-bin-win-cuda-13.3-x64.zip"]);
        assert_eq!(
            assets.variant("vulkan").map(|a| a.asset()),
            Some("llama-{tag}-bin-win-vulkan-x64.zip"),
            "a bare per-variant string normalises to an asset with no extras"
        );
        assert_eq!(
            latest.binaries["llama-server"]
                .resolver_asset("aarch64-apple-darwin")
                .and_then(|a| a.variant(CPU_VARIANT))
                .map(|a| a.asset()),
            Some("llama-{tag}-bin-macos-arm64.tar.gz"),
            "a bare triple string is the cpu variant"
        );

        let mut stable = published_stable_manifest();
        stable["signature"] = serde_json::json!("x");
        let stable = parse_manifest(&stable).unwrap();
        let BinaryManifestEntry::Pinned { platforms, .. } = &stable.binaries["llama-server"] else {
            panic!("pinned");
        };
        let cuda = platforms["x86_64-pc-windows-msvc"].variant("cuda").unwrap();
        assert_eq!(cuda.extras.as_ref().map(|e| e.len()), Some(1));
        assert!(
            platforms["aarch64-apple-darwin"]
                .variant(CPU_VARIANT)
                .is_some(),
            "a bare pinned target is the cpu variant"
        );
    }

    #[test]
    fn an_entry_in_neither_shape_does_not_fail_the_document() {
        // The oracle never looks inside `binaries` (`manifest.ts:106-127`), so
        // a kind published in a third shape leaves it working. Failing the
        // whole load here would take `deno` down with it and stop every tool
        // worker on a core that had no reason to care.
        let mut document = published_stable_manifest();
        document["signature"] = serde_json::json!("x");
        document["binaries"]["something-new"] = serde_json::json!({ "channels": ["beta"] });

        let manifest = parse_manifest(&document).unwrap();

        assert!(matches!(
            manifest.binaries["something-new"],
            BinaryManifestEntry::Unknown(_)
        ));
        assert!(matches!(
            manifest.binaries["deno"],
            BinaryManifestEntry::Pinned { .. }
        ));
    }

    #[test]
    fn the_version_a_plain_list_reports_never_needs_the_network() {
        let mut stable = published_stable_manifest();
        stable["signature"] = serde_json::json!("x");
        let stable = parse_manifest(&stable).unwrap();
        let mut latest = published_latest_manifest();
        latest["signature"] = serde_json::json!("x");
        let latest = parse_manifest(&latest).unwrap();

        // Pinned: the manifest's own version, installed or not.
        assert_eq!(
            stable.binaries["deno"].version_without_network(None),
            "v2.8.2"
        );
        // Resolver: the installed one, or the literal "latest".
        assert_eq!(
            latest.binaries["deno"].version_without_network(None),
            "latest"
        );
        assert_eq!(
            latest.binaries["deno"].version_without_network(Some("v2.8.1")),
            "v2.8.1"
        );
    }

    // -- signature ----------------------------------------------------------

    #[test]
    fn a_document_signed_over_its_canonical_form_verifies() {
        let (signing, public) = keypair();
        let raw = signed(stable_at("1.2.3"), &signing);

        let manifest = verify_document(&raw, &public).unwrap();

        assert_eq!(manifest.version, "1.2.3");
    }

    #[test]
    fn a_bad_signature_is_refused() {
        let (signing, public) = keypair();
        let raw = signed(stable_at("1.2.3"), &signing);
        let mut document: Value = serde_json::from_str(&raw).unwrap();
        document["signature"] =
            Value::String(base64::engine::general_purpose::STANDARD.encode([9u8; 64]));

        let err = verify_document(&document.to_string(), &public).unwrap_err();

        assert_eq!(err.code, ErrorCode::SignatureInvalid);
    }

    #[test]
    fn the_signed_version_is_authenticated_alongside_the_binaries() {
        // The signature covers `version`, which is what makes the replay
        // comparison meaningful: an attacker who could edit the version freely
        // could defeat the refusal by claiming to be newer.
        let (signing, public) = keypair();
        let raw = signed(stable_at("1.2.3"), &signing);
        let mut document: Value = serde_json::from_str(&raw).unwrap();
        document["version"] = serde_json::json!("9.9.9");

        assert!(verify_document(&document.to_string(), &public).is_err());
    }

    #[test]
    fn a_document_with_a_tampered_url_is_refused() {
        let (signing, public) = keypair();
        let raw = signed(stable_at("1.2.3"), &signing);
        let mut document: Value = serde_json::from_str(&raw).unwrap();
        document["binaries"]["deno"]["platforms"][crate::config::host_triple()]["url"] =
            serde_json::json!("https://attacker.invalid/deno.zip");

        assert!(verify_document(&document.to_string(), &public).is_err());
    }

    #[test]
    fn a_field_this_core_does_not_model_still_verifies() {
        // The finding this fixes: canonicalising a re-serialisation of the
        // struct drops whatever the struct does not model, so the FIRST field a
        // future release adds breaks verification on every deployed core and
        // reads as an attack rather than as schema drift.
        let (signing, public) = keypair();
        let mut document = stable_at("1.2.3");
        document["publishedAtMs"] = serde_json::json!(1_770_000_000_000i64);
        document["binaries"]["deno"]["notes"] = serde_json::json!("a field from a later release");
        let raw = signed(document, &signing);

        verify_document(&raw, &public)
            .expect("an unmodelled field was treated as a forged signature");
    }

    #[test]
    fn a_manifest_missing_its_signature_or_version_is_refused_at_the_shape_check() {
        let mut value = published_stable_manifest();
        let err = parse_manifest(&value).unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
        assert!(err.message.contains("signature"), "{}", err.message);

        value["signature"] = serde_json::json!("abc");
        value["version"] = serde_json::json!("");
        let err = parse_manifest(&value).unwrap_err();
        assert!(err.message.contains("version"), "{}", err.message);

        value["version"] = serde_json::json!("1.2.3");
        value["schemaVersion"] = serde_json::json!(2);
        let err = parse_manifest(&value).unwrap_err();
        assert!(err.message.contains("schemaVersion"), "{}", err.message);

        let err = parse_manifest(&serde_json::json!(null)).unwrap_err();
        assert!(err.message.contains("not an object"), "{}", err.message);
    }

    // -- load, cache, replay ------------------------------------------------

    #[tokio::test]
    async fn a_dev_core_builds_its_manifest_with_no_network() {
        // The consent rule at the manifest layer: a dev boot resolves from the
        // config compiled into it and must not reach out at all.
        let net = Net::new().unwrap();
        let cancel = CancellationToken::new();
        let dir = tempfile::tempdir().unwrap();

        let m = bounded(load_manifest(
            &net,
            Channel::Dev,
            dir.path(),
            "http://127.0.0.1:1/binaries.json",
            true,
            &cancel,
        ))
        .await
        .unwrap();

        assert_eq!(net.calls(), 0, "a dev manifest reached the network");
        let BinaryManifestEntry::Resolver { resolver } = &m.binaries["deno"] else {
            panic!("a dev core resolves at runtime, like latest");
        };
        assert_eq!(
            resolver.pinned_tag.as_deref(),
            Some(crate::sidecars::deno::PINNED_DENO_TAG),
            "the dev manifest must pin the same tag the rest of the core does"
        );
        assert!(
            resolver.assets.contains_key(crate::config::host_triple()),
            "a dev manifest that omits this host resolves nothing"
        );
    }

    #[tokio::test]
    async fn a_good_cache_short_circuits_the_fetch() {
        // What lets a boot install a missing binary offline.
        let _trust = Trusted::new();
        let dir = tempfile::tempdir().unwrap();
        write_cached(dir.path(), &_trust.sign(stable_at("1.2.3")))
            .await
            .unwrap();
        let net = Net::new().unwrap();

        let loaded = bounded(load_manifest(
            &net,
            Channel::Stable,
            dir.path(),
            "http://127.0.0.1:1/binaries.json",
            false,
            &CancellationToken::new(),
        ))
        .await
        .unwrap();

        assert_eq!(loaded.version, "1.2.3");
        assert_eq!(net.calls(), 0, "a fresh cache was refetched");
    }

    #[tokio::test]
    async fn a_tampered_cache_is_ignored() {
        // The cache is re-verified on every READ. A cache trusted because it is
        // ours is a file any local process can rewrite.
        let _trust = Trusted::new();
        let dir = tempfile::tempdir().unwrap();
        let raw = _trust.sign(stable_at("1.2.3"));
        let mut document: Value = serde_json::from_str(&raw).unwrap();
        document["binaries"]["deno"]["platforms"][crate::config::host_triple()]["url"] =
            serde_json::json!("https://attacker.invalid/deno.zip");
        write_cached(dir.path(), &document.to_string())
            .await
            .unwrap();

        assert!(
            cached_manifest(dir.path()).await.is_none(),
            "a rewritten cache was accepted"
        );
    }

    #[tokio::test]
    async fn a_truncated_cache_is_ignored_rather_than_fatal() {
        let _trust = Trusted::new();
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path()).await.unwrap();
        tokio::fs::write(cache_path(dir.path()), b"{\"schemaVersion\":1,")
            .await
            .unwrap();

        assert!(cached_manifest(dir.path()).await.is_none());
    }

    #[tokio::test]
    async fn an_older_signed_manifest_is_refused_and_the_cache_kept() {
        // A validly-signed OLDER manifest is still validly signed, so serving
        // it would roll binaries back to a release with a fixed vulnerability.
        //
        // Driven end to end through the real `load_manifest`. The previous
        // version of this test asserted `compare_semver` directly and never
        // called the function whose refusal it named, so the branch could be
        // deleted with `if false &&` and the suite stayed green (mutation 46).
        let _trust = Trusted::new();
        let dir = tempfile::tempdir().unwrap();
        write_cached(dir.path(), &_trust.sign(stable_at("1.10.0")))
            .await
            .unwrap();
        let server = Upstream::start(Serve::Json(_trust.sign(stable_at("1.2.0")))).await;
        let net = Net::new().unwrap();

        let loaded = bounded(load_manifest(
            &net,
            Channel::Stable,
            dir.path(),
            &server.url("/binaries.json"),
            true,
            &CancellationToken::new(),
        ))
        .await
        .unwrap();

        assert_eq!(
            loaded.version, "1.10.0",
            "a replayed older manifest replaced the cache"
        );
        assert_eq!(
            net.calls(),
            1,
            "the refusal happens after the fetch, not instead of it"
        );
        // And the cache on disk is untouched, so the next boot is not rolled
        // back either.
        assert_eq!(cached_manifest(dir.path()).await.unwrap().version, "1.10.0");
    }

    #[tokio::test]
    async fn a_same_version_manifest_is_accepted_rather_than_refused() {
        // The comparison is `< 0`, not `<= 0` (`manifest.ts:54`). A re-publish
        // at the same version is a republish, not a rollback, and refusing it
        // would pin a core to whatever it happened to cache first.
        let _trust = Trusted::new();
        let dir = tempfile::tempdir().unwrap();
        write_cached(dir.path(), &_trust.sign(stable_at("1.2.3")))
            .await
            .unwrap();
        let mut republished = stable_at("1.2.3");
        republished["binaries"]["deno"]["version"] = serde_json::json!("v2.9.0");
        let server = Upstream::start(Serve::Json(_trust.sign(republished))).await;
        let net = Net::new().unwrap();

        let loaded = bounded(load_manifest(
            &net,
            Channel::Stable,
            dir.path(),
            &server.url("/binaries.json"),
            true,
            &CancellationToken::new(),
        ))
        .await
        .unwrap();

        let BinaryManifestEntry::Pinned { version, .. } = &loaded.binaries["deno"] else {
            panic!("pinned");
        };
        assert_eq!(version, "v2.9.0", "a same-version republish was refused");
    }

    #[tokio::test]
    async fn a_newer_signed_manifest_replaces_the_cache() {
        let _trust = Trusted::new();
        let dir = tempfile::tempdir().unwrap();
        write_cached(dir.path(), &_trust.sign(stable_at("1.2.3")))
            .await
            .unwrap();
        let server = Upstream::start(Serve::Json(_trust.sign(stable_at("1.10.0")))).await;
        let net = Net::new().unwrap();

        let loaded = bounded(load_manifest(
            &net,
            Channel::Stable,
            dir.path(),
            &server.url("/binaries.json"),
            true,
            &CancellationToken::new(),
        ))
        .await
        .unwrap();

        assert_eq!(loaded.version, "1.10.0");
        assert_eq!(
            cached_manifest(dir.path()).await.unwrap().version,
            "1.10.0",
            "the cache was not updated"
        );
    }

    #[tokio::test]
    async fn a_manifest_signed_by_another_key_is_refused_and_never_cached() {
        let _trust = Trusted::new();
        let dir = tempfile::tempdir().unwrap();
        let stranger = SigningKey::from_bytes(&[9u8; 32]);
        let server = Upstream::start(Serve::Json(signed(stable_at("1.2.3"), &stranger))).await;
        let net = Net::new().unwrap();

        let err = bounded(load_manifest(
            &net,
            Channel::Stable,
            dir.path(),
            &server.url("/binaries.json"),
            true,
            &CancellationToken::new(),
        ))
        .await
        .unwrap_err();

        assert_eq!(err.code, ErrorCode::SignatureInvalid);
        assert!(cached_manifest(dir.path()).await.is_none());
    }

    #[tokio::test]
    async fn a_non_2xx_manifest_fetch_is_a_manifest_fetch_failure() {
        let server = Upstream::start(Serve::Status(503)).await;
        let net = Net::new().unwrap();
        let cancel = CancellationToken::new();
        let dir = tempfile::tempdir().unwrap();

        let err = bounded(load_manifest(
            &net,
            Channel::Stable,
            dir.path(),
            &server.url("/binaries.json"),
            true,
            &cancel,
        ))
        .await
        .unwrap_err();

        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
    }
}
