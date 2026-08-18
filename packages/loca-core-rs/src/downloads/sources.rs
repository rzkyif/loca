//! Source-string parsing and the size probe.
//!
//! One source format: `@user/repo/branch/filename`. It maps to
//! `user/repo/filename` under the destination root (the branch is addressing,
//! not layout) and to the HuggingFace resolve URL.
//!
//! This module decides where bytes land on disk, so its per-component check is a
//! security control rather than input hygiene: `relPath` is joined onto a root
//! that holds every model the user owns.

use std::path::{Path, PathBuf};

use crate::contract::api::DownloadPlan;
use crate::downloads::cancel::CancellationToken;
use crate::downloads::net::{Net, Redirect};
use crate::error::{validation, Result};

/// Whether a probe may reach the network for a size hint.
///
/// An enum rather than the `bool` the TypeScript passes, because this is the one
/// boolean in the codebase whose two answers differ by "does this contact
/// HuggingFace". `probe(sources, false)` read backwards is a silent consent
/// violation; `Probe::LocalOnly` read backwards does not compile into the wrong
/// thing. The `bool` survives only at D6's already-shipped trait boundary, where
/// it is converted on the first line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// Presence only. The always-on requirements snapshot's mode.
    LocalOnly,
    /// Presence, plus a HEAD for the size of anything missing.
    WithSizes,
}

impl Probe {
    pub fn from_network_flag(network: bool) -> Self {
        if network {
            Self::WithSizes
        } else {
            Self::LocalOnly
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSource {
    pub rel_path: String,
    pub url: String,
    pub filename: String,
}

/// Reject a path component that would let `rel_path` escape the destination root
/// when joined: traversal, empties, NUL, or a separator where one segment is
/// expected.
///
/// The backslash rejection is not Windows pedantry: `a\..\..\etc` is one
/// component to a POSIX split and three to the Windows path API, so a component
/// that passes this check on one platform and traverses on the other is exactly
/// the defect this refuses.
fn assert_safe_component(component: &str, source: &str) -> Result<()> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains('\0')
        || component.contains('\\')
    {
        return Err(validation(format!("unsafe source component in {source}")));
    }
    Ok(())
}

pub fn parse_source(source: &str, hf_base_url: &str) -> Result<ParsedSource> {
    if !source.starts_with('@') {
        // A bare path. Accepted so a caller can name a file that is already on
        // disk; not downloadable, which the empty URL is how the transfer learns.
        let filename = source.rsplit('/').next().unwrap_or(source).to_string();
        return Ok(ParsedSource {
            rel_path: source.to_string(),
            url: String::new(),
            filename,
        });
    }
    let parts: Vec<&str> = source[1..].split('/').collect();
    if parts.len() < 4 {
        return Err(validation(format!("invalid source format: {source}")));
    }
    let (username, reponame, branchname) = (parts[0], parts[1], parts[2]);
    assert_safe_component(username, source)?;
    assert_safe_component(reponame, source)?;
    assert_safe_component(branchname, source)?;
    // The filename may legitimately be a nested repo path (`subdir/model.gguf`),
    // so each segment is checked rather than the joined string: a check on the
    // join sees `a/../../etc` as one unfamiliar name and passes it.
    let filename = parts[3..].join("/");
    for segment in filename.split('/') {
        assert_safe_component(segment, source)?;
    }
    let base_name = filename.rsplit('/').next().unwrap_or(&filename).to_string();
    Ok(ParsedSource {
        rel_path: format!("{username}/{reponame}/{filename}"),
        url: format!(
            "{hf_base_url}/{username}/{reponame}/resolve/{branchname}/{filename}?download=true"
        ),
        filename: base_name,
    })
}

#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub source: String,
    pub url: String,
    pub filename: String,
    pub size_bytes: Option<u64>,
    pub already_have: bool,
    pub abs_path: PathBuf,
}

impl ProbeResult {
    pub fn to_plan(&self) -> DownloadPlan {
        DownloadPlan {
            source: self.source.clone(),
            already_have: self.already_have,
            size_hint: self.size_bytes.map(|n| n as i64),
            // A probe knows neither: `error` is the requirements snapshot's own
            // carry-through and `version` belongs to a binary plan. Absent, not
            // defaulted, because the contract omits them when unset and a
            // present-but-empty answer is a different one (D-028).
            error: None,
            version: None,
        }
    }
}

/// Presence, and for a missing file in [`Probe::WithSizes`] a size hint.
///
/// The size HEAD is the only outbound call here, and it is skipped three ways:
/// a non-`@` source has no upstream, a file already on disk needs no estimate,
/// and [`Probe::LocalOnly`] forbids it outright. That last one is what the
/// always-on requirements snapshot passes, so this function is on the path the
/// consent rule is really protecting.
pub async fn probe_source(
    net: &Net,
    source: &str,
    destination_root: &Path,
    hf_base_url: &str,
    mode: Probe,
    cancel: &CancellationToken,
) -> Result<ProbeResult> {
    let parsed = parse_source(source, hf_base_url)?;
    let abs_path = destination_root.join(&parsed.rel_path);
    let already_have = tokio::fs::metadata(&abs_path).await.is_ok();

    let mut result = ProbeResult {
        source: source.to_string(),
        url: parsed.url,
        filename: parsed.filename,
        size_bytes: None,
        already_have,
        abs_path,
    };
    if !source.starts_with('@') || already_have || mode == Probe::LocalOnly {
        return Ok(result);
    }
    result.size_bytes = probe_size(net, &result.url, cancel).await;
    Ok(result)
}

/// The size of `url`, or `None` when it cannot be established.
///
/// A HuggingFace resolve URL 302s to a CDN that often omits `content-length` on
/// a HEAD, while the 302 itself carries `x-linked-size`. So the redirect is read
/// first and the followed HEAD is the fallback, not the other way round.
///
/// Every failure is swallowed: a probe is an estimate for a progress bar, and a
/// caller that cannot show one still has to be able to offer the download.
/// A manual HEAD that ERRORS ends the probe rather than falling through to the
/// followed one: the oracle wraps both in one `try` (`sources.ts:93-127`), so a
/// failing first HEAD skips the second. Trying anyway would make an extra
/// outbound request per failed probe, and would answer a size where the oracle
/// answers none.
async fn probe_size(net: &Net, url: &str, cancel: &CancellationToken) -> Option<u64> {
    let head = net.head(url, Redirect::Manual, cancel).await.ok()?;
    if let Some(linked) = head.header_u64("x-linked-size") {
        return Some(linked);
    }
    // Zero means the REDIRECT's own body rather than the resource's. A 302
    // carries no body and plenty of servers say so explicitly, so taking 0
    // here reports the file as empty and skips the fallback below that
    // would have found its real size.
    match head.header_u64("content-length") {
        Some(length) if length > 0 => return Some(length),
        _ => {}
    }
    net.head(url, Redirect::Follow, cancel)
        .await
        .ok()?
        .header_u64("content-length")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::downloads::testing::{bounded, Serve, Upstream};
    use crate::error::ErrorCode;

    const HF: &str = "https://huggingface.co";

    #[test]
    fn the_on_disk_path_is_user_repo_file_with_no_branch() {
        let parsed = parse_source("@u/r/main/model.gguf", HF).unwrap();
        assert_eq!(parsed.rel_path, "u/r/model.gguf");
        assert_eq!(
            parsed.url,
            "https://huggingface.co/u/r/resolve/main/model.gguf?download=true"
        );
        assert_eq!(parsed.filename, "model.gguf");
    }

    #[test]
    fn a_nested_filename_keeps_its_directories_and_reports_the_base_name() {
        let parsed = parse_source("@u/r/main/sub/dir/model.gguf", HF).unwrap();
        assert_eq!(parsed.rel_path, "u/r/sub/dir/model.gguf");
        assert_eq!(
            parsed.url,
            "https://huggingface.co/u/r/resolve/main/sub/dir/model.gguf?download=true"
        );
        assert_eq!(
            parsed.filename, "model.gguf",
            "the filename is the base name; the relative path keeps the directories"
        );
    }

    #[test]
    fn a_traversing_component_is_refused() {
        for source in [
            "@../r/main/f.gguf",
            "@u/../main/f.gguf",
            "@u/r/../f.gguf",
            "@u/r/main/../../../etc/passwd",
            "@./r/main/f.gguf",
        ] {
            let err = parse_source(source, HF).unwrap_err();
            assert_eq!(err.code, ErrorCode::ValidationError, "{source}");
            assert!(err.message.contains("unsafe source component"), "{source}");
        }
    }

    #[test]
    fn a_nested_filename_is_checked_segment_by_segment() {
        // The join reads as one unfamiliar name and passes; the segments do not.
        let err = parse_source("@u/r/main/sub/../../../etc/passwd", HF).unwrap_err();
        assert!(err.message.contains("unsafe source component"));
    }

    #[test]
    fn a_backslash_component_is_refused() {
        // One component to a POSIX split, three to the Windows path API.
        let err = parse_source("@u/r/main/sub\\..\\..\\evil", HF).unwrap_err();
        assert!(err.message.contains("unsafe source component"));
        let err = parse_source("@u\\..\\../r/main/f", HF).unwrap_err();
        assert!(err.message.contains("unsafe source component"));
    }

    #[test]
    fn a_nul_component_is_refused() {
        let err = parse_source("@u/r/main/model\0.gguf", HF).unwrap_err();
        assert!(err.message.contains("unsafe source component"));
    }

    #[test]
    fn an_empty_component_is_refused() {
        let err = parse_source("@u//main/f.gguf", HF).unwrap_err();
        assert!(err.message.contains("unsafe source component"));
        let err = parse_source("@u/r/main/sub//f.gguf", HF).unwrap_err();
        assert!(err.message.contains("unsafe source component"));
    }

    #[test]
    fn too_few_segments_is_an_invalid_format_rather_than_an_unsafe_component() {
        let err = parse_source("@u/r/main", HF).unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(
            err.message.contains("invalid source format"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_bare_path_parses_as_present_but_not_downloadable() {
        let parsed = parse_source("/abs/path/model.gguf", HF).unwrap();
        assert_eq!(parsed.rel_path, "/abs/path/model.gguf");
        assert_eq!(
            parsed.url, "",
            "an empty URL is how the transfer refuses it"
        );
        assert_eq!(parsed.filename, "model.gguf");
    }

    #[test]
    fn the_base_url_is_honoured_so_a_mirror_is_reachable() {
        let parsed = parse_source("@u/r/main/f.gguf", "http://127.0.0.1:9999").unwrap();
        assert!(parsed
            .url
            .starts_with("http://127.0.0.1:9999/u/r/resolve/main/"));
    }

    #[tokio::test]
    async fn probing_locally_makes_no_outbound_call() {
        // The consent rule's own test: this is the mode the always-on
        // requirements snapshot uses, and it must never reach a host.
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();

        let probe = bounded(probe_source(
            &net,
            "@u/r/main/model.gguf",
            dir.path(),
            HF,
            Probe::LocalOnly,
            &cancel,
        ))
        .await
        .unwrap();

        assert_eq!(net.calls(), 0, "a local-only probe reached the network");
        assert!(!probe.already_have);
        assert_eq!(probe.size_bytes, None);
        assert_eq!(probe.abs_path, dir.path().join("u/r/model.gguf"));
    }

    #[tokio::test]
    async fn a_file_already_on_disk_is_not_probed_upstream() {
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("u/r")).unwrap();
        std::fs::write(dir.path().join("u/r/model.gguf"), b"here").unwrap();
        let cancel = CancellationToken::new();

        let probe = bounded(probe_source(
            &net,
            "@u/r/main/model.gguf",
            dir.path(),
            HF,
            Probe::WithSizes,
            &cancel,
        ))
        .await
        .unwrap();

        assert!(probe.already_have);
        assert_eq!(probe.size_bytes, None, "presence needs no size estimate");
        assert_eq!(net.calls(), 0, "a file on disk was probed upstream anyway");
    }

    #[tokio::test]
    async fn the_302_carries_the_real_size_and_the_cdn_does_not() {
        // x-linked-size is the resource; the redirect's own content-length is
        // whatever the resolve endpoint answered with, which is not it.
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let server = Upstream::start(Serve::Status(200))
            .await
            .with_head_redirect(
                "/cdn",
                &[("x-linked-size", "4294967296"), ("content-length", "12")],
            );

        let probe = bounded(probe_source(
            &net,
            "@u/r/main/model.gguf",
            dir.path(),
            server.base(),
            Probe::WithSizes,
            &cancel,
        ))
        .await
        .unwrap();

        assert_eq!(probe.size_bytes, Some(4_294_967_296));
        assert_eq!(net.calls(), 1, "the 302 answered, so no fallback HEAD ran");
    }

    #[tokio::test]
    async fn a_redirect_that_declares_a_zero_length_falls_back_rather_than_believing_it() {
        // A 302 carries no body, and plenty of servers say `content-length: 0`
        // explicitly, so a zero here describes the REDIRECT rather than the
        // resource. Believing it reports the file as empty and skips the
        // fallback that would have found its real size.
        //
        // The oracle had this wrong and was fixed alongside: it tested the
        // truthiness of the STRING (`sources.ts:105`) and "0" is truthy, so a
        // declared zero ended the probe. Fixed in both, because a fix on one
        // side only would be a divergence from the thing this port is checked
        // against.
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let server = Upstream::start(Serve::Status(200))
            .await
            .with_head_redirect("/cdn", &[("content-length", "0")])
            .with_head_headers(&[("content-length", "777")]);

        let probe = bounded(probe_source(
            &net,
            "@u/r/main/model.gguf",
            dir.path(),
            server.base(),
            Probe::WithSizes,
            &cancel,
        ))
        .await
        .unwrap();

        assert_eq!(
            probe.size_bytes,
            Some(777),
            "a zero on the 302 must not be mistaken for the resource's size"
        );
        assert_eq!(net.calls(), 2, "the manual HEAD, then the followed one");
    }

    #[tokio::test]
    async fn a_redirect_with_no_size_falls_back_to_the_followed_head() {
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let server = Upstream::start(Serve::Status(200))
            .await
            .with_head_redirect("/cdn", &[])
            .with_head_headers(&[("content-length", "777")]);

        let probe = bounded(probe_source(
            &net,
            "@u/r/main/model.gguf",
            dir.path(),
            server.base(),
            Probe::WithSizes,
            &cancel,
        ))
        .await
        .unwrap();

        assert_eq!(probe.size_bytes, Some(777));
        assert_eq!(net.calls(), 2, "the manual HEAD, then the followed one");
    }

    #[tokio::test]
    async fn a_manual_head_that_fails_ends_the_probe_rather_than_trying_again() {
        // The oracle wraps both HEADs in ONE try (`sources.ts:93-127`), so a
        // failing first one skips the second. Falling through costs an extra
        // outbound request per failed probe, and reports a size where the
        // oracle reports none, which is a wire difference rather than a tidiness
        // one.
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();

        let probe = bounded(probe_source(
            &net,
            "@u/r/main/model.gguf",
            dir.path(),
            // Port 1 never listens, so the HEAD fails at connect.
            "http://127.0.0.1:1",
            Probe::WithSizes,
            &cancel,
        ))
        .await
        .unwrap();

        assert_eq!(probe.size_bytes, None);
        assert_eq!(
            net.calls(),
            1,
            "a failed manual HEAD was followed by a second request"
        );
    }

    #[tokio::test]
    async fn a_probe_failure_is_a_missing_estimate_rather_than_an_error() {
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();

        let probe = bounded(probe_source(
            &net,
            "@u/r/main/model.gguf",
            dir.path(),
            "http://127.0.0.1:1",
            Probe::WithSizes,
            &cancel,
        ))
        .await
        .unwrap();

        assert_eq!(probe.size_bytes, None);
        assert!(
            !probe.already_have,
            "the row is still offered without a size"
        );
    }

    #[test]
    fn a_plan_carries_the_size_hint_it_has_and_omits_the_one_it_does_not() {
        let probe = ProbeResult {
            source: "@u/r/main/f.gguf".into(),
            url: "https://x/y".into(),
            filename: "f.gguf".into(),
            size_bytes: Some(1024),
            already_have: false,
            abs_path: PathBuf::from("/tmp/f.gguf"),
        };
        let plan = probe.to_plan();
        assert_eq!(plan.source, "@u/r/main/f.gguf");
        assert!(!plan.already_have);
        assert_eq!(plan.size_hint, Some(1024));

        let plan = ProbeResult {
            size_bytes: None,
            already_have: true,
            ..probe
        }
        .to_plan();
        assert!(plan.already_have);
        assert_eq!(plan.size_hint, None);
    }
}
