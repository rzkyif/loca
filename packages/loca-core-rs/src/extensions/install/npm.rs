//! The npm registry client: three endpoints, none of them reachable from boot.
//!
//! Ports `packages/loca-core/src/extensions/npm-registry.ts`.
//!
//! | endpoint                    | backs                                     |
//! | --------------------------- | ----------------------------------------- |
//! | `/-/v1/search`              | the extensions browser's search field     |
//! | `/{name}` (dist-tags only)  | `POST /extensions/check-updates`          |
//! | `/{name}` (a version entry) | a user-triggered download or update       |
//!
//! Every one is a user action, which is why they may fetch at all. See
//! [`super::net`] for the property and how it is held.

use serde::Deserialize;

use crate::contract::api::{ExtensionSearchResult, ExtensionSearchResultSource};
use crate::error::{AppError, ErrorCode, Result};

use super::net::Http;

pub const REGISTRY_BASE: &str = "https://registry.npmjs.org";

/// Only packages carrying this keyword are offered. An npm-wide search would
/// surface tens of thousands of packages that cannot be installed as
/// extensions, and the user would find that out one failed install at a time.
const SEARCH_KEYWORD: &str = "loca-extension";

/// One resolved version, enough to fetch and verify its tarball.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmPackageVersion {
    pub version: String,
    pub tarball_url: String,
    /// npm's legacy `dist.shasum`, which is SHA-1 and weak.
    pub shasum: Option<String>,
    /// npm's `dist.integrity`, a Subresource-Integrity string
    /// (`sha512-<base64>`). The strong hash the tarball is verified against.
    pub integrity: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
}

/// Search for installable extensions.
pub async fn search(
    http: &dyn Http,
    query: &str,
    limit: u32,
    offset: u32,
) -> Result<Vec<ExtensionSearchResult>> {
    let text = format!("keywords:{SEARCH_KEYWORD} {query}");
    let url = format!(
        "{REGISTRY_BASE}/-/v1/search?text={}&size={limit}&from={offset}",
        form_encode(text.trim())
    );
    let body = http.get_text(&url).await?;
    let parsed: SearchBody = parse(&body, &url)?;
    Ok(parsed
        .objects
        .into_iter()
        .map(|entry| ExtensionSearchResult {
            name: entry.package.name,
            description: entry.package.description.unwrap_or_default(),
            version: entry.package.version,
            weekly_downloads: entry.downloads.and_then(|d| d.weekly),
            source: ExtensionSearchResultSource::Npm,
            homepage: entry.package.links.and_then(|l| l.homepage),
            license: entry.package.license.as_ref().and_then(as_text),
        })
        .collect())
}

/// The `dist-tags.latest` of a package, for an update check.
///
/// Deliberately lighter than [`resolve_version`]: an update check runs over
/// every installed extension in turn and needs one string from each, so it does
/// not resolve a tarball URL or its integrity metadata.
pub async fn resolve_latest_version(http: &dyn Http, name: &str) -> Result<String> {
    let url = metadata_url(name);
    let body = http.get_text(&url).await?;
    let meta: Metadata = parse(&body, &url)?;
    meta.dist_tags.get("latest").cloned().ok_or_else(|| {
        AppError::new(
            ErrorCode::ExtensionNotFound,
            format!("no latest version for {name}"),
        )
    })
}

/// Resolve a package, optionally at a requested version, to its tarball.
pub async fn resolve_version(
    http: &dyn Http,
    name: &str,
    version_spec: Option<&str>,
) -> Result<NpmPackageVersion> {
    let url = metadata_url(name);
    let body = http.get_text(&url).await?;
    let meta: Metadata = parse(&body, &url)?;
    let latest = meta.dist_tags.get("latest");
    // A caret range is not resolved here: npm's metadata carries no answer for
    // one, and the alternative would be a semver range solver over every
    // published version. `^1.2.3` therefore means "the current latest", which is
    // what the user pressing Update wants and what the specification does.
    let requested = match version_spec {
        Some(spec) if !spec.starts_with('^') => Some(spec.to_string()),
        _ => None,
    };
    let version = requested.or_else(|| latest.cloned()).ok_or_else(|| {
        AppError::new(
            ErrorCode::ExtensionNotFound,
            format!("no version for {name}"),
        )
    })?;
    let entry = meta.versions.get(&version).ok_or_else(|| {
        AppError::new(
            ErrorCode::ExtensionNotFound,
            format!("no metadata for {name}@{version}"),
        )
    })?;
    Ok(NpmPackageVersion {
        version,
        tarball_url: entry.dist.tarball.clone(),
        shasum: entry.dist.shasum.clone(),
        integrity: entry.dist.integrity.clone(),
        description: entry.description.clone(),
        homepage: entry.homepage.as_ref().and_then(as_text),
        license: entry.license.as_ref().and_then(as_text),
    })
}

fn metadata_url(name: &str) -> String {
    format!("{REGISTRY_BASE}/{}", uri_component(name))
}

fn parse<'a, T: Deserialize<'a>>(body: &'a str, url: &str) -> Result<T> {
    serde_json::from_str(body).map_err(|err| {
        AppError::new(
            ErrorCode::ManifestFetchFailed,
            format!("could not read the npm response from {url}: {err}"),
        )
    })
}

/// `license` and `homepage` are strings in the npm schema and are not always
/// strings in the npm registry: packages old enough predate it and publish
/// `{"type":"MIT","url":"..."}` for a license.
///
/// Typing them as `String` makes the whole metadata document unparseable for
/// those packages, so one badly-published dependency of an unrelated package
/// would make an extension impossible to install. They are read as JSON and
/// narrowed here instead, and a non-string is simply absent.
fn as_text(value: &serde_json::Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

/// `encodeURIComponent`, for one path segment.
///
/// A scoped package name contains `@` and `/`, and the `/` is the one that
/// matters: unencoded it turns `@scope/pkg` into two path segments, and the
/// registry answers 404 for a package that exists.
fn uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if is_unreserved(byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The `application/x-www-form-urlencoded` serialiser, for one query value.
///
/// Space becomes `+` rather than `%20`, which is what `URLSearchParams`
/// produces and what the search text needs: the query is
/// `keywords:loca-extension <terms>` and the space between them is a separator
/// npm's search parses.
fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b' ' => out.push('+'),
            b if b.is_ascii_alphanumeric() || matches!(b, b'*' | b'-' | b'.' | b'_') => {
                out.push(b as char)
            }
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
        )
}

#[derive(Debug, Deserialize)]
struct SearchBody {
    #[serde(default)]
    objects: Vec<SearchEntry>,
}

#[derive(Debug, Deserialize)]
struct SearchEntry {
    package: SearchPackage,
    #[serde(default)]
    downloads: Option<SearchDownloads>,
}

#[derive(Debug, Deserialize)]
struct SearchPackage {
    name: String,
    version: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    license: Option<serde_json::Value>,
    #[serde(default)]
    links: Option<SearchLinks>,
}

#[derive(Debug, Deserialize)]
struct SearchLinks {
    #[serde(default)]
    homepage: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchDownloads {
    #[serde(default)]
    weekly: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    #[serde(rename = "dist-tags", default)]
    dist_tags: std::collections::HashMap<String, String>,
    #[serde(default)]
    versions: std::collections::HashMap<String, VersionEntry>,
}

#[derive(Debug, Deserialize)]
struct VersionEntry {
    dist: Dist,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    homepage: Option<serde_json::Value>,
    #[serde(default)]
    license: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Dist {
    tarball: String,
    #[serde(default)]
    shasum: Option<String>,
    #[serde(default)]
    integrity: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use async_trait::async_trait;

    /// Answers a canned body and records every URL it was asked for.
    ///
    /// The URL is half of what these functions do, so a double that only
    /// returned a body would leave the encoding, the keyword filter and the
    /// paging untested while every assertion about the parsed result passed.
    struct Canned {
        body: String,
        urls: Mutex<Vec<String>>,
    }

    impl Canned {
        fn new(body: impl Into<String>) -> Self {
            Self {
                body: body.into(),
                urls: Mutex::new(Vec::new()),
            }
        }
        fn url(&self) -> String {
            self.urls
                .lock()
                .unwrap()
                .first()
                .cloned()
                .unwrap_or_default()
        }
    }

    #[async_trait]
    impl Http for Canned {
        async fn get_text(&self, url: &str) -> Result<String> {
            self.urls.lock().unwrap().push(url.to_string());
            Ok(self.body.clone())
        }
        async fn get_bytes(&self, _url: &str) -> Result<Vec<u8>> {
            panic!("these three endpoints are documents, not tarballs");
        }
    }

    fn metadata_body() -> String {
        serde_json::json!({
            "dist-tags": { "latest": "2.1.0", "next": "3.0.0-rc.1" },
            "versions": {
                "1.0.0": {
                    "dist": { "tarball": "https://registry.npmjs.org/x/-/x-1.0.0.tgz", "shasum": "aa" }
                },
                "2.1.0": {
                    "dist": {
                        "tarball": "https://registry.npmjs.org/x/-/x-2.1.0.tgz",
                        "shasum": "bb",
                        "integrity": "sha512-Zm9v"
                    },
                    "description": "the newer one",
                    "homepage": "https://example.test",
                    "license": "MIT"
                }
            }
        })
        .to_string()
    }

    #[tokio::test]
    async fn the_search_url_carries_the_keyword_filter_the_query_and_the_page() {
        // Losing the keyword turns this into an npm-wide search: every result
        // still renders, and every one of them fails to install.
        let http = Canned::new(r#"{"objects":[]}"#);
        search(&http, "audio tools", 25, 50).await.unwrap();
        let url = http.url();
        assert!(
            url.contains("text=keywords%3Aloca-extension+audio+tools"),
            "unexpected search url: {url}"
        );
        assert!(url.contains("size=25"), "unexpected search url: {url}");
        assert!(url.contains("from=50"), "unexpected search url: {url}");
    }

    #[tokio::test]
    async fn an_empty_query_still_filters_by_the_keyword_and_leaves_no_trailing_space() {
        let http = Canned::new(r#"{"objects":[]}"#);
        search(&http, "", 10, 0).await.unwrap();
        let url = http.url();
        assert!(
            url.contains("text=keywords%3Aloca-extension&"),
            "an empty query must not leave a dangling separator: {url}"
        );
    }

    #[tokio::test]
    async fn a_search_result_carries_every_field_the_browser_shows() {
        let http = Canned::new(
            serde_json::json!({
                "objects": [{
                    "package": {
                        "name": "loca-extension-weather",
                        "version": "1.4.0",
                        "description": "the weather",
                        "license": "Apache-2.0",
                        "links": { "homepage": "https://weather.test" }
                    },
                    "downloads": { "weekly": 412 }
                }]
            })
            .to_string(),
        );
        let results = search(&http, "weather", 50, 0).await.unwrap();
        assert_eq!(results.len(), 1);
        let first = &results[0];
        assert_eq!(first.name, "loca-extension-weather");
        assert_eq!(first.version, "1.4.0");
        assert_eq!(first.description, "the weather");
        assert_eq!(first.license.as_deref(), Some("Apache-2.0"));
        assert_eq!(first.homepage.as_deref(), Some("https://weather.test"));
        assert_eq!(first.weekly_downloads, Some(412));
        assert_eq!(first.source, ExtensionSearchResultSource::Npm);
    }

    #[tokio::test]
    async fn a_result_missing_its_optional_fields_is_still_a_result() {
        let http = Canned::new(r#"{"objects":[{"package":{"name":"x","version":"0.1.0"}}]}"#);
        let results = search(&http, "", 50, 0).await.unwrap();
        assert_eq!(results[0].description, "");
        assert_eq!(results[0].homepage, None);
        assert_eq!(results[0].weekly_downloads, None);
    }

    #[tokio::test]
    async fn a_scoped_name_is_encoded_into_one_path_segment() {
        // Unencoded, `@acme/thing` is two path segments and the registry
        // answers 404 for a package that exists, which reads as "not published".
        let http = Canned::new(metadata_body());
        resolve_latest_version(&http, "@acme/thing").await.unwrap();
        assert_eq!(
            http.url(),
            "https://registry.npmjs.org/%40acme%2Fthing",
            "a scoped package name must be one encoded segment"
        );
    }

    #[tokio::test]
    async fn an_unscoped_name_is_not_mangled_by_the_encoder() {
        let http = Canned::new(metadata_body());
        resolve_latest_version(&http, "loca-extension-builtin")
            .await
            .unwrap();
        assert_eq!(
            http.url(),
            "https://registry.npmjs.org/loca-extension-builtin"
        );
    }

    #[tokio::test]
    async fn the_latest_lookup_reads_the_latest_tag_and_not_another_one() {
        // `next` is 3.0.0-rc.1 and deliberately higher: reading "the largest
        // version" or "the first tag" would offer a release candidate as an
        // update to every user.
        let http = Canned::new(metadata_body());
        assert_eq!(resolve_latest_version(&http, "x").await.unwrap(), "2.1.0");
    }

    #[tokio::test]
    async fn a_package_with_no_latest_tag_is_reported_as_missing() {
        let http = Canned::new(r#"{"versions":{"1.0.0":{"dist":{"tarball":"t"}}}}"#);
        let err = resolve_latest_version(&http, "x").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ExtensionNotFound);
    }

    #[tokio::test]
    async fn no_requested_version_resolves_to_latest_with_its_integrity() {
        let http = Canned::new(metadata_body());
        let resolved = resolve_version(&http, "x", None).await.unwrap();
        assert_eq!(resolved.version, "2.1.0");
        assert_eq!(
            resolved.tarball_url,
            "https://registry.npmjs.org/x/-/x-2.1.0.tgz"
        );
        assert_eq!(resolved.integrity.as_deref(), Some("sha512-Zm9v"));
        assert_eq!(resolved.shasum.as_deref(), Some("bb"));
        assert_eq!(resolved.license.as_deref(), Some("MIT"));
    }

    #[tokio::test]
    async fn an_exact_requested_version_wins_over_latest() {
        let http = Canned::new(metadata_body());
        let resolved = resolve_version(&http, "x", Some("1.0.0")).await.unwrap();
        assert_eq!(resolved.version, "1.0.0");
        assert_eq!(
            resolved.integrity, None,
            "1.0.0 publishes no integrity, and inventing one would make the \
             verification gate look satisfied"
        );
    }

    #[tokio::test]
    async fn a_caret_range_resolves_to_latest_rather_than_being_looked_up_verbatim() {
        // `^1.0.0` is not a key in `versions`, so treating it as an exact
        // request answers "no metadata for x@^1.0.0" for a package that is
        // installable.
        let http = Canned::new(metadata_body());
        let resolved = resolve_version(&http, "x", Some("^1.0.0")).await.unwrap();
        assert_eq!(resolved.version, "2.1.0");
    }

    #[tokio::test]
    async fn a_requested_version_that_was_never_published_is_reported_as_missing() {
        let http = Canned::new(metadata_body());
        let err = resolve_version(&http, "x", Some("9.9.9"))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ExtensionNotFound);
        assert!(err.message.contains("9.9.9"), "{}", err.message);
    }

    #[tokio::test]
    async fn an_object_shaped_license_leaves_the_package_installable() {
        // Packages old enough publish `{"type":"MIT"}`. Typing the field as a
        // String makes the whole document unparseable, so the extension cannot
        // be installed at all over a field nothing depends on.
        let http = Canned::new(
            serde_json::json!({
                "dist-tags": { "latest": "1.0.0" },
                "versions": {
                    "1.0.0": {
                        "dist": { "tarball": "t", "shasum": "aa" },
                        "license": { "type": "MIT", "url": "https://x.test" }
                    }
                }
            })
            .to_string(),
        );
        let resolved = resolve_version(&http, "x", None).await.unwrap();
        assert_eq!(resolved.version, "1.0.0");
        assert_eq!(resolved.license, None);
    }

    #[tokio::test]
    async fn an_unreadable_registry_response_is_an_error_rather_than_an_empty_list() {
        // An empty list reads to the user as "no extensions match", which is a
        // wrong answer where an error is a correct one.
        let http = Canned::new("<!doctype html><html>maintenance</html>");
        let err = search(&http, "x", 50, 0).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
    }

    #[test]
    fn the_two_encoders_differ_where_it_matters() {
        assert_eq!(uri_component("@acme/thing"), "%40acme%2Fthing");
        assert_eq!(uri_component("a b"), "a%20b");
        assert_eq!(form_encode("a b"), "a+b");
        assert_eq!(form_encode("keywords:x"), "keywords%3Ax");
        // Non-ASCII goes out as UTF-8 percent-encoded bytes, not as a lone char.
        assert_eq!(uri_component("é"), "%C3%A9");
    }
}
