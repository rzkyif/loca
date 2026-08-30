//! Loading the model catalog: the dev payload, the signed fetch, the verified
//! cache, and the pinned-hash index.
//!
//! Mirrors the binary-manifest path: fetch `catalog.json`, verify its Ed25519
//! signature against the baked-in public key over the whole payload minus
//! `signature`, and cache the verified copy at
//! `<root>/cache/model-catalog.json`. The cache is re-verified on every read,
//! which makes it a performance shortcut rather than a trust anchor, and lets a
//! re-check work offline against the last good fetch.
//!
//! The dev channel compiles the catalog instead of fetching it: there is no
//! published `manifests/dev/catalog.json` and the from-source build is the trust
//! anchor, so a dev core uses `catalog.dev.json`, generated from
//! `@loca/model-catalog` by `deno task contract` with no signature at all.

use std::collections::HashMap;

use serde_json::Value;

use crate::config::{Channel, Config};
use crate::contract::inbound::{CatalogModel, CatalogPayload, ScoreSelector, SpeechQuant};
use crate::error::{internal, AppError, ErrorCode, Result};
use crate::files;
use crate::paths::Paths;

use super::canonical::{canonicalize, verify_ed25519};

/// The compiled dev-channel catalog. See the module docs.
const DEV_CATALOG: &str = include_str!("catalog.dev.json");

use crate::config::SIGNING_PUBLIC_KEY;

const CACHE_FILE: &str = "model-catalog.json";

/// One model plus the family name it was listed under, which the browser shows
/// and the payload keeps one level up.
pub struct FlatModel<'a> {
    pub model: &'a CatalogModel,
    pub family: &'a str,
}

/// Every model across every family, in catalog order. Both the fit engine and
/// the browser iterate models rather than families.
///
/// A free function rather than a method: an inherent impl on a generated type
/// becomes a conflict the moment a regeneration adds one of its own.
pub fn flat_models(payload: &CatalogPayload) -> Vec<FlatModel<'_>> {
    payload
        .families
        .iter()
        .flat_map(|f| {
            f.models.iter().map(move |m| FlatModel {
                model: m,
                family: &f.family,
            })
        })
        .collect()
}

/// The primary "smartness" score of one model, or `None` when it carries no
/// score for the configured selector. `None` excludes the model from every
/// bucket, which is what keeps an unscored model browsable but never
/// recommended.
pub fn primary_score(model: &CatalogModel, sel: &ScoreSelector) -> Option<f64> {
    model
        .scores
        .iter()
        .flatten()
        .find(|s| s.source == sel.source && s.metric == sel.metric)
        .map(|s| s.value)
}

/// The `.min(1)` bounds the generated `Deserialize` cannot enforce. Everything
/// else the schema states is enforced by deserializing, so nothing here repeats
/// it. These bounds are load-bearing: `fit.rs` reads "at least one variant with
/// at least one quant" as a guarantee.
///
/// REFERENTIAL integrity is deliberately NOT checked. Duplicate speech ids and a
/// preset pointing at a missing model or quant are accepted, and the pickers
/// skip a dangling card and serve the rest; rejecting the whole payload here
/// would answer `manifest_fetch_failed` across the entire models surface.
///
/// The error is a plain `String` so the caller picks the code: a bad fetched
/// catalog is `manifest_fetch_failed`, a bad embedded one is a build defect.
pub fn validate(payload: &CatalogPayload) -> Result<(), String> {
    if payload.families.is_empty() {
        return Err("catalog has no families".into());
    }
    for family in &payload.families {
        if family.models.is_empty() {
            return Err(format!("family {} has no models", family.family));
        }
        for model in &family.models {
            if model.variants.is_empty() {
                return Err(format!("model {} has no variants", model.id));
            }
            for variant in &model.variants {
                if variant.quants.is_empty() {
                    return Err(format!(
                        "variant {} of {} has no quants",
                        variant.label, model.id
                    ));
                }
            }
        }
    }
    check_speech(
        "stt",
        payload.stt.models.iter().map(|m| (&m.id, &m.quants)),
        payload.stt.presets.is_empty(),
    )?;
    check_speech(
        "tts",
        payload.tts.models.iter().map(|m| (&m.id, &m.quants)),
        payload.tts.presets.is_empty(),
    )?;
    for model in &payload.tts.models {
        if model.voices.is_empty() {
            return Err(format!("tts model {} has no voices", model.id));
        }
    }
    Ok(())
}

/// The `.min(1)` bounds for one speech catalog, shared by both: only the models'
/// ids and quants, and the presets' emptiness, are read. Referential integrity
/// is deliberately NOT checked; see [`validate`].
fn check_speech<'a>(
    kind: &str,
    models: impl Iterator<Item = (&'a String, &'a Vec<SpeechQuant>)>,
    presets_empty: bool,
) -> Result<(), String> {
    let mut count = 0usize;
    for (id, quants) in models {
        count += 1;
        if quants.is_empty() {
            return Err(format!("{kind} model {id} has no quants"));
        }
        for quant in quants {
            if quant.files.is_empty() {
                return Err(format!(
                    "{kind} model {id} quant {} has no files",
                    quant.quant
                ));
            }
        }
    }
    if count == 0 {
        return Err(format!("{kind} catalog has no models"));
    }
    if presets_empty {
        return Err(format!("{kind} catalog has no presets"));
    }
    Ok(())
}

/// The embedded dev payload, with `generatedAt` stamped now: the committed file
/// carries an empty string, so two loads a second apart legitimately report
/// different values.
pub fn dev_payload() -> Result<CatalogPayload> {
    let file: Value = serde_json::from_str(DEV_CATALOG)
        .map_err(|err| internal(format!("the embedded dev catalog is not JSON: {err}")))?;
    let mut payload: CatalogPayload = serde_json::from_value(file["payload"].clone())
        .map_err(|err| internal(format!("the embedded dev catalog does not parse: {err}")))?;
    validate(&payload)
        .map_err(|err| internal(format!("the embedded dev catalog is invalid: {err}")))?;
    payload.generated_at = now_iso8601();
    Ok(payload)
}

/// `new Date().toISOString()`: UTC, milliseconds, `Z`.
fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Load the catalog: cached and verified unless `force`, otherwise fetch,
/// verify and cache.
pub async fn load(config: &Config, paths: &Paths, force: bool) -> Result<CatalogPayload> {
    if config.channel == Channel::Dev {
        return dev_payload();
    }
    if !force {
        if let Some(cached) = read_cached(paths).await {
            return Ok(cached);
        }
    }
    let raw = fetch(config).await?;
    let payload = parse_and_verify(&raw)?;
    write_cached(paths, &raw).await?;
    Ok(payload)
}

async fn fetch(config: &Config) -> Result<String> {
    let url = config.models_catalog_url();
    let fetch_failed = |what: String| AppError::new(ErrorCode::ManifestFetchFailed, what);

    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|err| fetch_failed(format!("failed to fetch model catalog: {err}")))?;
    if !response.status().is_success() {
        return Err(fetch_failed(format!(
            "model catalog HTTP {}",
            response.status()
        )));
    }
    response
        .text()
        .await
        .map_err(|err| fetch_failed(format!("failed to read model catalog: {err}")))
}

/// Parse, validate and verify one catalog document.
///
/// The signature covers `canonicalize(payload)`, the whole document minus
/// `signature`, taken from the RAW JSON rather than re-serialised from the typed
/// value: the published bytes already carry every schema default, and
/// re-serialising would be a second chance to differ.
fn parse_and_verify(raw: &str) -> Result<CatalogPayload> {
    let fetch_failed = |what: String| AppError::new(ErrorCode::ManifestFetchFailed, what);

    let mut document: Value = serde_json::from_str(raw)
        .map_err(|err| fetch_failed(format!("invalid catalog JSON: {err}")))?;
    let Some(object) = document.as_object_mut() else {
        return Err(fetch_failed("catalog is not an object".into()));
    };
    let Some(Value::String(signature)) = object.remove("signature") else {
        return Err(fetch_failed("catalog has no signature".into()));
    };

    let payload: CatalogPayload = serde_json::from_value(document.clone())
        .map_err(|err| fetch_failed(format!("catalog schema invalid: {err}")))?;
    validate(&payload).map_err(|err| fetch_failed(format!("catalog schema invalid: {err}")))?;

    verify_ed25519(
        SIGNING_PUBLIC_KEY,
        &signature,
        canonicalize(&document).as_bytes(),
    )
    .map_err(|_| {
        AppError::new(
            ErrorCode::SignatureInvalid,
            "model catalog signature verification failed",
        )
    })?;
    Ok(payload)
}

fn cache_path(paths: &Paths) -> std::path::PathBuf {
    paths.cache_dir().join(CACHE_FILE)
}

/// The cached copy, re-verified. Any failure returns `None` so the caller
/// fetches: an unreadable or tampered cache is a cache miss, never an error the
/// user sees.
async fn read_cached(paths: &Paths) -> Option<CatalogPayload> {
    let path = cache_path(paths);
    let text = tokio::fs::read_to_string(&path).await.ok()?;
    match parse_and_verify(&text) {
        Ok(payload) => Some(payload),
        Err(err) => {
            tracing::warn!(
                target: "loca::catalog",
                "ignoring unreadable or invalid catalog cache: {}", err.message
            );
            None
        }
    }
}

/// Cache the verified bytes exactly as fetched, so the next read verifies the
/// same signature over the same message.
///
/// `write_atomic`, not the fixed-`.tmp` writer: that name exists only so the
/// extension sandbox's deny list can name a transient, and this file is not on
/// it.
async fn write_cached(paths: &Paths, raw: &str) -> Result<()> {
    tokio::fs::create_dir_all(paths.cache_dir()).await?;
    files::write_atomic(cache_path(paths), raw.as_bytes().to_vec()).await
}

/// Index every downloadable file's spec to its pinned sha256, so a model
/// download verifies its bytes against the SIGNED catalog instead of trusting
/// HuggingFace's `x-linked-etag`.
pub fn build_sha256_index(payload: &CatalogPayload) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for family in &payload.families {
        for model in &family.models {
            for variant in &model.variants {
                if let (Some(spec), Some(sha)) = (&variant.mmproj_spec, &variant.mmproj_sha256) {
                    out.insert(spec.clone(), sha.to_string());
                }
                for quant in &variant.quants {
                    if let Some(sha) = &quant.sha256 {
                        out.insert(quant.model_spec.clone(), sha.to_string());
                    }
                }
            }
        }
    }
    let stt = payload.stt.models.iter().map(|m| &m.quants);
    let tts = payload.tts.models.iter().map(|m| &m.quants);
    for quants in stt.chain(tts) {
        for quant in quants {
            for file in &quant.files {
                if let Some(sha) = &file.sha256 {
                    out.insert(file.model_spec.clone(), sha.to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Every downloadable file in the shipped catalog carries a pinned hash.
    /// Without one the download path falls back to HuggingFace's published
    /// hash, which is a weaker guarantee than the signed catalog's.
    #[test]
    fn every_downloadable_file_carries_a_pinned_sha256() {
        let payload = dev_payload().expect("the dev catalog loads");
        let is_sha = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());

        let mut specs = std::collections::BTreeSet::new();
        for family in &payload.families {
            for model in &family.models {
                for variant in &model.variants {
                    if let Some(spec) = &variant.mmproj_spec {
                        let sha = variant
                            .mmproj_sha256
                            .as_ref()
                            .unwrap_or_else(|| panic!("missing mmproj sha256 for {spec}"));
                        assert!(is_sha(sha), "{spec}");
                        specs.insert(spec.clone());
                    }
                    for quant in &variant.quants {
                        let sha = quant
                            .sha256
                            .as_ref()
                            .unwrap_or_else(|| panic!("missing sha256 for {}", quant.model_spec));
                        assert!(is_sha(sha), "{}", quant.model_spec);
                        specs.insert(quant.model_spec.clone());
                    }
                }
            }
        }
        for quants in payload
            .stt
            .models
            .iter()
            .map(|m| &m.quants)
            .chain(payload.tts.models.iter().map(|m| &m.quants))
        {
            for quant in quants {
                for file in &quant.files {
                    let sha = file
                        .sha256
                        .as_ref()
                        .unwrap_or_else(|| panic!("missing sha256 for {}", file.model_spec));
                    assert!(is_sha(sha), "{}", file.model_spec);
                    specs.insert(file.model_spec.clone());
                }
            }
        }

        // The index covers exactly the distinct spec set: a spec reused across
        // variants is one entry, and nothing downloadable is missing from it.
        let index = build_sha256_index(&payload);
        assert_eq!(index.len(), specs.len());
        for spec in &specs {
            assert!(index.contains_key(spec), "{spec} is indexed");
        }
        // A spec that is not in the catalog is a custom user pick, and the
        // download path falls back to HuggingFace's hash for it.
        assert!(!index.contains_key("@someone/custom-repo/main/model.gguf"));
    }

    /// A 64-character string is not enough: the digest has to be lowercase HEX,
    /// because it is compared byte for byte against the downloaded file.
    #[test]
    fn a_sha256_outside_the_hex_alphabet_is_a_schema_violation() {
        let payload = dev_payload().expect("loads");
        for bad in [
            "z".repeat(64),                 // lowercase, and not hex
            "A".repeat(64),                 // hex, and not lowercase
            format!("{}0", "a".repeat(64)), // 65 characters
            "a".repeat(63),                 // 63 characters
        ] {
            let mut document = serde_json::to_value(&payload).expect("serialises");
            document["families"][0]["models"][0]["variants"][0]["quants"][0]["sha256"] =
                serde_json::json!(bad);
            document["signature"] = serde_json::json!(base64_of_64_zero_bytes());
            let err = parse_and_verify(&document.to_string()).expect_err("rejected");
            assert_eq!(err.code, ErrorCode::ManifestFetchFailed, "{bad}");
        }

        // The same 64 characters in the alphabet the regex allows are accepted,
        // so the assertions above are about the alphabet rather than the field.
        let mut document = serde_json::to_value(&payload).expect("serialises");
        document["families"][0]["models"][0]["variants"][0]["quants"][0]["sha256"] =
            serde_json::json!("f".repeat(64));
        document["signature"] = serde_json::json!(base64_of_64_zero_bytes());
        let err =
            parse_and_verify(&document.to_string()).expect_err("the signature is still wrong");
        assert_eq!(
            err.code,
            ErrorCode::SignatureInvalid,
            "it got past the schema and failed on the signature instead"
        );
    }

    #[test]
    fn the_dev_payload_is_stamped_per_load() {
        let payload = dev_payload().expect("loads");
        assert!(
            payload.generated_at.ends_with('Z') && payload.generated_at.contains('.'),
            "toISOString shape with milliseconds, got {}",
            payload.generated_at
        );
        assert!(!payload.families.is_empty());
    }

    /// Verification is over the raw document minus `signature`, so a tampered
    /// field or a missing signature is rejected before anything reads it. The
    /// happy path cannot be tested here: only the release key can sign, and
    /// `canonical.rs` covers the signature machinery against its own fixture.
    #[test]
    fn an_unsigned_or_tampered_catalog_is_rejected() {
        let payload = dev_payload().expect("loads");
        let mut document = serde_json::to_value(&payload).expect("serialises");

        let err = parse_and_verify(&document.to_string()).expect_err("no signature");
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);

        document["signature"] = serde_json::json!(base64_of_64_zero_bytes());
        let err = parse_and_verify(&document.to_string()).expect_err("a wrong signature");
        assert_eq!(err.code, ErrorCode::SignatureInvalid);

        // A schema violation is reported as a fetch failure, matching the
        // TypeScript's `safeParse` branch, and is caught BEFORE the signature
        // so a malformed catalog cannot reach the verifier.
        document["families"] = serde_json::json!([]);
        let err = parse_and_verify(&document.to_string()).expect_err("no families");
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);

        let err = parse_and_verify("{").expect_err("not JSON");
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
        let err = parse_and_verify("[]").expect_err("not an object");
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
    }

    fn test_config(channel: Channel) -> Config {
        Config {
            channel,
            host: "127.0.0.1".to_string(),
            port: 0,
            version: crate::config::CORE_VERSION,
            storage_base_url: "https://get.loca.rzkyif.com".to_string(),
            hf_base_url: "https://huggingface.co".to_string(),
        }
    }

    fn base64_of_64_zero_bytes() -> String {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine as _;
        STANDARD.encode([0u8; 64])
    }

    /// An unknown field is rejected rather than ignored. The catalog is a
    /// signed artifact, not a client request: a field this core does not
    /// understand means a catalog from the future, and half-applying it is
    /// worse than refusing it.
    #[test]
    fn an_unknown_catalog_field_is_rejected() {
        let payload = dev_payload().expect("loads");
        let mut document = serde_json::to_value(&payload).expect("serialises");
        document["somethingNew"] = serde_json::json!(true);
        document["signature"] = serde_json::json!(base64_of_64_zero_bytes());
        let err = parse_and_verify(&document.to_string()).expect_err("rejected");
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
        assert!(err.message.contains("somethingNew"), "{}", err.message);
    }

    #[tokio::test]
    async fn the_dev_channel_never_reads_or_writes_the_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::rooted(dir.path());
        let config = test_config(Channel::Dev);

        let payload = load(&config, &paths, false).await.expect("dev loads");
        assert!(!payload.families.is_empty());
        assert!(
            !cache_path(&paths).exists(),
            "the dev channel has nothing to cache: the source IS the anchor"
        );
    }

    #[tokio::test]
    async fn an_unverifiable_cache_is_ignored_rather_than_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::rooted(dir.path());
        tokio::fs::create_dir_all(paths.cache_dir())
            .await
            .expect("mkdir");
        tokio::fs::write(cache_path(&paths), "not json at all")
            .await
            .expect("write");
        assert!(read_cached(&paths).await.is_none());

        // A well-formed catalog with no valid signature is equally a miss: the
        // cache is a shortcut, never a second trust anchor.
        let payload = dev_payload().expect("loads");
        let mut document = serde_json::to_value(&payload).expect("serialises");
        document["signature"] = serde_json::json!(base64_of_64_zero_bytes());
        tokio::fs::write(cache_path(&paths), document.to_string())
            .await
            .expect("write");
        assert!(read_cached(&paths).await.is_none());
    }
}
