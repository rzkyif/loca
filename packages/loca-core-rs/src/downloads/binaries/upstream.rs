//! Resolving a binary's download to a concrete URL plus sha256.
//!
//! Two sources, matching the two published entry shapes (see
//! [`super::manifest`]): a PINNED entry already carries the URL and the hash the
//! release resolved, and a RESOLVER entry names a GitHub repo plus per-triple
//! asset patterns that are resolved here at runtime against the release's own
//! published digest.
//!
//! # Pinned tags only, and the loud refusal that keeps that true
//!
//! `deno` pins `pinnedTag`, so the resolver branch fetches exactly one release
//! by tag. The unpinned branch of the oracle (list recent releases, filter
//! drafts and prereleases, scan newest-first for the first complete asset) is
//! dropped with the rest of plan section 4.2.
//!
//! That branch is dead **only because of a data value in another package**. If
//! `pinnedTag` is removed from the `deno` entry, a core with the scan deleted
//! resolves nothing, the tool workers never install, and there is no compile
//! error anywhere to say so. So an entry with no pinned tag is refused here with
//! a message naming [`PINNED_DENO_TAG`], which is the cheap mitigation the plan
//! chose over keeping 120 lines of newest-first scanning alive for a case that
//! must not happen.
//!
//! # An unusable asset is `None`, not an error
//!
//! Absent, still uploading, or lacking a sha256 digest all resolve to `None` so
//! the caller can say "not available yet" rather than failing the whole install.
//! **An undigested asset is never returned**: without the digest the download
//! cannot be verified, and an unverifiable binary is not installable.
//!
//! # The GitHub host is overridable
//!
//! `https://api.github.com` was hardcoded in both cores, so
//! `/api/v1/binaries/{check,probe}` reached the real internet during a
//! conformance run, contradicting `tests/conformance/harness/upstream.ts:6-9`,
//! which states that every base URL points at 127.0.0.1 precisely so a core that
//! escapes fails its cases instead of quietly succeeding. `LOCA_GITHUB_API_BASE_URL`
//! closes that, next to the `LOCA_HF_BASE_URL` and `LOCA_STORAGE_BASE_URL` that
//! already existed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::downloads::binaries::manifest::{BinaryManifestEntry, CPU_VARIANT};
use crate::downloads::cancel::CancellationToken;
use crate::downloads::net::Net;
use crate::error::{AppError, ErrorCode, Result};
use crate::sidecars::deno::PINNED_DENO_TAG;

const DEFAULT_GITHUB_API_BASE_URL: &str = "https://api.github.com";

/// The GitHub API host, overridable so a test harness can serve it locally.
pub fn github_api_base_url() -> String {
    std::env::var("LOCA_GITHUB_API_BASE_URL")
        .ok()
        .map(|value| value.trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_GITHUB_API_BASE_URL.to_string())
}

/// What a resolved release asset gives the installer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBinary {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: Option<u64>,
}

/// The upstream half of a manifest entry: which repo, which tag, which assets.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamResolver {
    pub repo: String,
    /// Keyed by triple. `{tag}` in a pattern is substituted with the release tag.
    #[serde(default)]
    pub assets: BTreeMap<String, TripleAsset>,
    #[serde(rename = "pinnedTag", default, skip_serializing_if = "Option::is_none")]
    pub pinned_tag: Option<String>,
}

/// A triple's asset config: a bare pattern (single-variant, read as `cpu`) or a
/// per-variant map. Normalised through [`TripleAsset::variant`], which is
/// `assetVariants` (`domain/model.ts:136-148`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum TripleAsset {
    /// A bare pattern. Read as the single `cpu` variant, which is what makes
    /// `deno` and the macOS llama builds need no special case anywhere.
    One(VariantAsset),
    Variants(BTreeMap<String, VariantAsset>),
}

impl TripleAsset {
    pub fn variant(&self, variant: &str) -> Option<&VariantAsset> {
        match self {
            Self::One(asset) => (variant == CPU_VARIANT).then_some(asset),
            Self::Variants(map) => map.get(variant),
        }
    }
}

/// One downloadable artifact: a primary asset-name pattern plus optional
/// companion archives (the Windows CUDA `cudart` runtime is the live example).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum VariantAsset {
    Name(String),
    Full {
        asset: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extra: Option<Vec<String>>,
    },
}

impl VariantAsset {
    pub fn asset(&self) -> &str {
        match self {
            Self::Name(name) => name,
            Self::Full { asset, .. } => asset,
        }
    }

    pub fn extra(&self) -> &[String] {
        match self {
            Self::Name(_) => &[],
            Self::Full { extra, .. } => extra.as_deref().unwrap_or(&[]),
        }
    }
}

/// Resolve a manifest entry to a concrete download for this host's `cpu` build.
///
/// `None` when this entry does not cover the triple or the upstream asset is not
/// installable yet, which is `resolveBinaryEntry` (`upstream-resolver.ts:235`)
/// plus `platformVariants`/`assetVariants` normalisation.
pub async fn resolve_entry(
    net: &Net,
    entry: &BinaryManifestEntry,
    triple: &str,
    cancel: &CancellationToken,
) -> Result<Option<ResolvedBinary>> {
    match entry {
        BinaryManifestEntry::Pinned { version, platforms } => {
            let Some(target) = platforms.get(triple).and_then(|p| p.variant(CPU_VARIANT)) else {
                return Ok(None);
            };
            Ok(Some(ResolvedBinary {
                version: version.clone(),
                url: target.url.clone(),
                sha256: target.sha256.clone(),
                // A pinned entry publishes no size; the caller HEADs for one
                // when it needs to show a number (`manager.ts:396`).
                size_bytes: None,
            }))
        }
        BinaryManifestEntry::Resolver { resolver } => {
            resolve_upstream(net, resolver, triple, cancel).await
        }
        // Neither published shape. See the manifest module docs: this is the
        // same answer the oracle gives for a triple an entry does not cover,
        // rather than an error that would take the whole route down.
        BinaryManifestEntry::Unknown(_) => Ok(None),
    }
}

/// Resolve `resolver`'s `cpu` asset for `triple` from its pinned release.
pub async fn resolve_upstream(
    net: &Net,
    resolver: &UpstreamResolver,
    triple: &str,
    cancel: &CancellationToken,
) -> Result<Option<ResolvedBinary>> {
    let Some(pattern) = asset_pattern(resolver, triple) else {
        // Upstream does not publish this triple, so it is unavailable rather
        // than missing (`upstream-resolver.ts:216-217`).
        return Ok(None);
    };
    let tag = resolver.pinned_tag.as_deref().filter(|t| !t.is_empty());
    let Some(tag) = tag else {
        // See the module docs: this is the tripwire for a data change in
        // another package that would otherwise fail silently.
        return Err(AppError::new(
            ErrorCode::ValidationError,
            format!(
                "upstream resolver for {} has no pinnedTag; this core resolves pinned releases \
                 only (see PINNED_DENO_TAG, currently {PINNED_DENO_TAG}). Restore the pin, or \
                 restore the newest-first release scan this core deliberately does not carry.",
                resolver.repo
            ),
        ));
    };

    let url = format!(
        "{}/repos/{}/releases/tags/{tag}",
        github_api_base_url(),
        resolver.repo
    );
    let release: GitHubRelease =
        serde_json::from_value(net.json(&url, cancel).await?).map_err(|err| {
            AppError::new(
                ErrorCode::ManifestFetchFailed,
                format!("could not read the release for {tag}: {err}"),
            )
        })?;
    Ok(resolve_asset_in_release(&release, &pattern))
}

/// The `cpu` asset-name pattern this resolver offers for `triple`.
fn asset_pattern(resolver: &UpstreamResolver, triple: &str) -> Option<String> {
    Some(
        resolver
            .assets
            .get(triple)?
            .variant(CPU_VARIANT)?
            .asset()
            .to_string(),
    )
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    /// Absent on older releases; `"uploaded"` once the upload settled.
    #[serde(default)]
    state: Option<String>,
    /// `"sha256:<hex>"`. Absent for a moment after upload, and on old releases.
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

/// The named, fully-uploaded, digest-carrying asset in this release, if any.
fn resolve_asset_in_release(release: &GitHubRelease, pattern: &str) -> Option<ResolvedBinary> {
    let wanted = pattern.replace("{tag}", &release.tag_name);
    let asset = release.assets.iter().find(|a| a.name == wanted)?;
    // An in-progress upload ("starter"/"new") is not resolvable yet. Absent
    // state means an older release that predates the field, which is fine.
    if asset.state.as_deref().is_some_and(|s| s != "uploaded") {
        return None;
    }
    // No sha256 digest means nothing can verify the bytes, so there is no
    // version of this that is safe to install.
    let digest = asset.digest.as_deref()?.strip_prefix("sha256:")?;
    Some(ResolvedBinary {
        version: release.tag_name.clone(),
        url: asset.browser_download_url.clone(),
        sha256: digest.to_string(),
        size_bytes: asset.size,
    })
}

/// Pointing the GitHub host somewhere else, for tests elsewhere in the crate.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub mod test_github {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Sets `LOCA_GITHUB_API_BASE_URL` for the life of the guard.
    ///
    /// Serialised on a mutex because the environment is process-global and
    /// cargo runs tests in threads: two of these racing would make each read
    /// the other's host, which is a flake that looks like a resolver bug.
    pub struct GithubHost(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl GithubHost {
        pub fn set(base: &str) -> Self {
            let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::set_var("LOCA_GITHUB_API_BASE_URL", base);
            Self(guard)
        }

        pub fn clear() -> Self {
            let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("LOCA_GITHUB_API_BASE_URL");
            Self(guard)
        }
    }

    impl Drop for GithubHost {
        fn drop(&mut self) {
            std::env::remove_var("LOCA_GITHUB_API_BASE_URL");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::test_github::GithubHost;
    use super::*;
    use crate::downloads::binaries::manifest::parse_manifest;
    use crate::downloads::testing::{bounded, published_latest_manifest, Serve, Upstream};

    fn make_release(assets: serde_json::Value) -> GitHubRelease {
        serde_json::from_value(serde_json::json!({
            "tag_name": "v2.8.2",
            "assets": assets,
        }))
        .unwrap()
    }

    fn asset(extra: serde_json::Value) -> serde_json::Value {
        let mut base = serde_json::json!({
            "name": "deno-aarch64-apple-darwin.zip",
            "browser_download_url": "https://example.invalid/deno.zip",
            "state": "uploaded",
            "digest": "sha256:abc123",
            "size": 4096,
        });
        for (key, value) in extra.as_object().unwrap() {
            if value.is_null() {
                base.as_object_mut().unwrap().remove(key);
            } else {
                base[key] = value.clone();
            }
        }
        base
    }

    const PATTERN: &str = "deno-aarch64-apple-darwin.zip";

    fn resolver(triple: &str, pattern: &str, pinned: Option<&str>) -> UpstreamResolver {
        let mut assets = BTreeMap::new();
        assets.insert(
            triple.to_string(),
            TripleAsset::One(VariantAsset::Name(pattern.to_string())),
        );
        UpstreamResolver {
            repo: "denoland/deno".into(),
            assets,
            pinned_tag: pinned.map(|t| t.to_string()),
        }
    }

    #[test]
    fn a_complete_asset_resolves_with_its_digest_and_size() {
        let release = make_release(serde_json::json!([asset(serde_json::json!({}))]));

        let resolved = resolve_asset_in_release(&release, PATTERN).unwrap();

        assert_eq!(resolved.version, "v2.8.2");
        assert_eq!(resolved.sha256, "abc123");
        assert_eq!(resolved.size_bytes, Some(4096));
        assert_eq!(resolved.url, "https://example.invalid/deno.zip");
    }

    #[test]
    fn an_asset_with_no_sha256_digest_is_refused() {
        // Without a digest nothing can verify the bytes, so there is no version
        // of installing this that is safe.
        let release = make_release(serde_json::json!([asset(
            serde_json::json!({ "digest": null })
        )]));
        assert_eq!(resolve_asset_in_release(&release, PATTERN), None);

        // A digest in another algorithm is not a sha256 and is refused too.
        let release = make_release(serde_json::json!([asset(
            serde_json::json!({ "digest": "sha512:abc123" })
        )]));
        assert_eq!(resolve_asset_in_release(&release, PATTERN), None);
    }

    #[test]
    fn an_asset_still_uploading_is_not_resolvable() {
        for state in ["starter", "new"] {
            let release = make_release(serde_json::json!([asset(
                serde_json::json!({ "state": state })
            )]));
            assert_eq!(resolve_asset_in_release(&release, PATTERN), None, "{state}");
        }
    }

    #[test]
    fn an_absent_state_field_is_an_older_release_rather_than_an_incomplete_one() {
        let release = make_release(serde_json::json!([asset(
            serde_json::json!({ "state": null })
        )]));
        assert!(resolve_asset_in_release(&release, PATTERN).is_some());
    }

    #[test]
    fn an_asset_that_is_not_in_the_release_is_none() {
        let release = make_release(serde_json::json!([asset(
            serde_json::json!({ "name": "deno-x86_64-pc-windows-msvc.zip" })
        )]));
        assert_eq!(resolve_asset_in_release(&release, PATTERN), None);
    }

    #[test]
    fn the_tag_placeholder_is_substituted_into_the_asset_name() {
        let release = make_release(serde_json::json!([asset(
            serde_json::json!({ "name": "deno-v2.8.2.zip" })
        )]));

        assert!(resolve_asset_in_release(&release, "deno-{tag}.zip").is_some());
    }

    #[test]
    fn the_cpu_pattern_is_read_from_either_asset_shape() {
        // A bare string is the cpu variant; a per-variant map is read by key.
        let bare = resolver("aarch64-apple-darwin", "deno.zip", Some("v1"));
        assert_eq!(
            asset_pattern(&bare, "aarch64-apple-darwin").as_deref(),
            Some("deno.zip")
        );
        assert_eq!(asset_pattern(&bare, "x86_64-apple-darwin"), None);

        let mut assets = BTreeMap::new();
        let mut variants = BTreeMap::new();
        variants.insert("cpu".to_string(), VariantAsset::Name("cpu.zip".into()));
        variants.insert(
            "cuda".to_string(),
            VariantAsset::Full {
                asset: "cuda.zip".into(),
                extra: Some(vec!["cudart.zip".into()]),
            },
        );
        assets.insert(
            "x86_64-pc-windows-msvc".to_string(),
            TripleAsset::Variants(variants),
        );
        let mapped = UpstreamResolver {
            repo: "r".into(),
            assets,
            pinned_tag: None,
        };
        assert_eq!(
            asset_pattern(&mapped, "x86_64-pc-windows-msvc").as_deref(),
            Some("cpu.zip"),
            "this core installs cpu, even where upstream offers gpu builds"
        );
    }

    #[tokio::test]
    async fn a_resolver_with_no_pinned_tag_is_refused_loudly() {
        // The whole reason this refusal exists: the unpinned scan is dead only
        // because of a data value in ANOTHER package, so removing that value
        // must fail here rather than silently resolve nothing.
        let net = Net::new().unwrap();
        let cancel = CancellationToken::new();
        let triple = crate::config::host_triple();
        for pinned in [None, Some("")] {
            let resolver = resolver(triple, "deno.zip", pinned);

            let err = resolve_upstream(&net, &resolver, triple, &cancel)
                .await
                .unwrap_err();

            assert_eq!(err.code, ErrorCode::ValidationError, "{pinned:?}");
            assert!(err.message.contains("pinnedTag"), "{}", err.message);
            assert!(
                err.message.contains("PINNED_DENO_TAG"),
                "the refusal has to name the constant to fix: {}",
                err.message
            );
            assert_eq!(net.calls(), 0, "it reached the network before refusing");
        }
    }

    #[tokio::test]
    async fn a_triple_upstream_does_not_publish_is_unavailable_rather_than_a_refusal() {
        // Checked BEFORE the pinned-tag refusal, matching
        // `upstream-resolver.ts:216-217`: a triple the resolver omits is
        // "upstream does not build this", not a misconfiguration.
        let net = Net::new().unwrap();
        let cancel = CancellationToken::new();
        let resolver = resolver("aarch64-apple-darwin", "deno.zip", None);

        let resolved = resolve_upstream(&net, &resolver, "riscv64-unknown-linux-gnu", &cancel)
            .await
            .unwrap();

        assert_eq!(resolved, None);
        assert_eq!(net.calls(), 0);
    }

    #[tokio::test]
    async fn a_pinned_entry_resolves_off_the_manifest_with_no_network() {
        // The stable channel's whole point: the release already resolved every
        // URL and hash, so installing needs no third party at all.
        let mut document = crate::downloads::testing::published_stable_manifest();
        document["signature"] = serde_json::json!("x");
        let manifest = parse_manifest(&document).unwrap();
        let net = Net::new().unwrap();
        let cancel = CancellationToken::new();
        let triple = crate::config::host_triple();

        let resolved = bounded(resolve_entry(
            &net,
            &manifest.binaries["deno"],
            triple,
            &cancel,
        ))
        .await
        .unwrap()
        .expect("stable pins this host's deno");

        assert_eq!(resolved.version, "v2.8.2");
        assert!(resolved.url.ends_with(&format!("deno-{triple}.zip")));
        assert_eq!(resolved.sha256.len(), 64);
        assert_eq!(
            resolved.size_bytes, None,
            "a pinned entry publishes no size; the caller HEADs for one"
        );
        assert_eq!(net.calls(), 0, "a pinned entry resolved over the network");
    }

    #[tokio::test]
    async fn a_resolver_entry_resolves_against_the_overridable_github_host() {
        // The `latest` half of the same route, and the proof that the hardcoded
        // api.github.com is gone: this test's release lives on 127.0.0.1 and it
        // would be unreachable if the host were still baked in.
        let triple = crate::config::host_triple();
        let release = serde_json::json!({
            "tag_name": "v2.8.2",
            "assets": [{
                "name": format!("deno-{triple}.zip"),
                "browser_download_url": "https://example.invalid/deno.zip",
                "state": "uploaded",
                "digest": "sha256:ABC123",
                "size": 4096,
            }],
        });
        let server = Upstream::start(Serve::Json(release.to_string())).await;
        let _guard = GithubHost::set(server.base());
        let mut document = published_latest_manifest();
        document["signature"] = serde_json::json!("x");
        let manifest = parse_manifest(&document).unwrap();
        let net = Net::new().unwrap();

        let resolved = bounded(resolve_entry(
            &net,
            &manifest.binaries["deno"],
            triple,
            &CancellationToken::new(),
        ))
        .await
        .unwrap()
        .expect("latest resolves this host's deno");

        assert_eq!(resolved.version, "v2.8.2");
        assert_eq!(resolved.sha256, "ABC123");
        assert_eq!(
            server.calls()[0].path,
            "/repos/denoland/deno/releases/tags/v2.8.2"
        );
    }

    #[test]
    fn the_github_host_defaults_to_the_real_one_and_trims_a_trailing_slash() {
        {
            let _guard = GithubHost::clear();
            assert_eq!(github_api_base_url(), "https://api.github.com");
        }
        {
            // Scoped separately: the guard holds the serialising lock, so
            // taking a second one while the first is alive would deadlock.
            let _guard = GithubHost::set("http://127.0.0.1:9/");
            assert_eq!(github_api_base_url(), "http://127.0.0.1:9");
        }
    }
}
