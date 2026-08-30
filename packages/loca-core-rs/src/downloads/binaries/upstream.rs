//! Resolving a binary's download to a concrete URL plus sha256: a PINNED entry
//! already carries both, a RESOLVER entry names a GitHub repo plus per-triple
//! asset patterns resolved here against the release's published digest.
//!
//! Only pinned tags are resolved. The unpinned newest-first scan is dead solely
//! because the `deno` entry pins `pinnedTag` in another package, so an entry
//! without one is refused loudly rather than silently resolving nothing.
//! An absent, still-uploading, or undigested asset is `None`, never an error;
//! an undigested asset is never returned because the bytes could not be verified.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::downloads::binaries::manifest::{BinaryManifestEntry, CPU_VARIANT};
use crate::downloads::cancel::CancellationToken;
use crate::downloads::net::Net;
use crate::error::{AppError, ErrorCode, Result};
use crate::sidecars::deno::PINNED_DENO_TAG;

const DEFAULT_GITHUB_API_BASE_URL: &str = "https://api.github.com";

/// The GitHub API host, overridable via `LOCA_GITHUB_API_BASE_URL` so a test
/// harness can serve it locally.
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
/// per-variant map.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum TripleAsset {
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
/// companion archives.
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
/// `None` when the entry does not cover the triple or the asset is not installable yet.
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
                // A pinned entry publishes no size; the caller HEADs for one.
                size_bytes: None,
            }))
        }
        BinaryManifestEntry::Resolver { resolver } => {
            resolve_upstream(net, resolver, triple, cancel).await
        }
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
        return Ok(None);
    };
    let tag = resolver.pinned_tag.as_deref().filter(|t| !t.is_empty());
    let Some(tag) = tag else {
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
    // Absent state is an older release that predates the field, which is fine.
    if asset.state.as_deref().is_some_and(|s| s != "uploaded") {
        return None;
    }
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

    /// Sets `LOCA_GITHUB_API_BASE_URL` for the life of the guard. Serialised on
    /// a mutex because the environment is process-global and tests run in threads.
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

    const PATTERN: &str = "deno-aarch64-apple-darwin.zip";

    /// Resolve `PATTERN` against a one-asset v2.8.2 release whose asset is the
    /// complete default overridden by `extra` (a `null` value removes the key).
    fn resolve_with(extra: serde_json::Value) -> Option<ResolvedBinary> {
        let mut base = serde_json::json!({
            "name": PATTERN,
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
        let release: GitHubRelease =
            serde_json::from_value(serde_json::json!({ "tag_name": "v2.8.2", "assets": [base] }))
                .unwrap();
        resolve_asset_in_release(&release, PATTERN)
    }

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
        let resolved = resolve_with(serde_json::json!({})).unwrap();

        assert_eq!(resolved.version, "v2.8.2");
        assert_eq!(resolved.sha256, "abc123");
        assert_eq!(resolved.size_bytes, Some(4096));
        assert_eq!(resolved.url, "https://example.invalid/deno.zip");
    }

    #[test]
    fn an_asset_with_no_sha256_digest_is_refused() {
        assert_eq!(resolve_with(serde_json::json!({ "digest": null })), None);
        assert_eq!(
            resolve_with(serde_json::json!({ "digest": "sha512:abc123" })),
            None
        );
    }

    #[test]
    fn an_asset_still_uploading_is_not_resolvable() {
        for state in ["starter", "new"] {
            assert_eq!(
                resolve_with(serde_json::json!({ "state": state })),
                None,
                "{state}"
            );
        }
    }

    #[test]
    fn an_absent_state_field_is_an_older_release_rather_than_an_incomplete_one() {
        assert!(resolve_with(serde_json::json!({ "state": null })).is_some());
    }

    #[test]
    fn an_asset_that_is_not_in_the_release_is_none() {
        assert_eq!(
            resolve_with(serde_json::json!({ "name": "deno-x86_64-pc-windows-msvc.zip" })),
            None
        );
    }

    #[test]
    fn the_tag_placeholder_is_substituted_into_the_asset_name() {
        let release: GitHubRelease = serde_json::from_value(serde_json::json!({
            "tag_name": "v2.8.2",
            "assets": [{
                "name": "deno-v2.8.2.zip",
                "browser_download_url": "https://example.invalid/deno.zip",
                "state": "uploaded",
                "digest": "sha256:abc123",
                "size": 4096,
            }],
        }))
        .unwrap();

        assert!(resolve_asset_in_release(&release, "deno-{tag}.zip").is_some());
    }

    #[test]
    fn the_cpu_pattern_is_read_from_either_asset_shape() {
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
            // Scoped separately: two live guards would deadlock on the lock.
            let _guard = GithubHost::set("http://127.0.0.1:9/");
            assert_eq!(github_api_base_url(), "http://127.0.0.1:9");
        }
    }
}
