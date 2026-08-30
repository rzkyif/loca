//! Download an artifact, decompress it, hash it as it streams, and place it.
//! Shared by the update path and the fresh-install path so both run one trust path.
//!
//! The manifest `sha256` is over the DECOMPRESSED bytes, so decompression
//! happens before hashing; the built-in extension tarball hashes as served,
//! which is why [`Decompress`] is a parameter. Nothing is held in memory: the
//! decoder is drained per chunk and the digest is incremental.
//! The temp name is a fixed `<out>.tmp`: only one apply is ever in flight (the
//! first exits the process) and staging is cleared before any write.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;

use crate::error::{AppError, ErrorCode, Result};
use crate::update::net::Net;

/// Whether the body is gunzipped on the way to disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decompress {
    /// Every artifact named in `core.json`.
    Gzip,
    /// The built-in extension tarball, whose published digest is over the
    /// tarball as served.
    None,
}

/// Stream `url` to `out_path`, returning the lowercase-hex sha256 of what was
/// written. The caller compares.
pub async fn stream_to_file(
    net: &Net,
    url: &str,
    out_path: &Path,
    decompress: Decompress,
) -> Result<String> {
    if let Some(parent) = out_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut stream = net.stream(url).await?;
    let mut file = tokio::fs::File::create(out_path).await?;
    let mut hasher = Sha256::new();
    // Push-based decoder over a Vec that is drained after every chunk.
    let mut decoder = flate2::write::GzDecoder::new(Vec::new());

    while let Some(chunk) = stream.next_chunk().await {
        let chunk = chunk?;
        match decompress {
            Decompress::None => {
                hasher.update(&chunk);
                file.write_all(&chunk).await?;
            }
            Decompress::Gzip => {
                decoder.write_all(&chunk).map_err(corrupt_gzip(url))?;
                let inflated = std::mem::take(decoder.get_mut());
                if !inflated.is_empty() {
                    hasher.update(&inflated);
                    file.write_all(&inflated).await?;
                }
            }
        }
    }

    if decompress == Decompress::Gzip {
        // Without this a truncated archive is written short and reported only
        // as a digest mismatch, which names the wrong cause.
        decoder.try_finish().map_err(corrupt_gzip(url))?;
        let inflated = std::mem::take(decoder.get_mut());
        if !inflated.is_empty() {
            hasher.update(&inflated);
            file.write_all(&inflated).await?;
        }
    }

    file.flush().await?;
    // Closed before the caller's rename, which cannot race an unflushed handle on Windows.
    drop(file);
    Ok(hex::encode(hasher.finalize()))
}

/// Download `url` to `out_path` and verify it against `sha256`. On any failure
/// the partial is destroyed so a later step cannot read refused bytes by name.
pub async fn download_and_verify(
    net: &Net,
    url: &str,
    out_path: &Path,
    sha256: &str,
    decompress: Decompress,
) -> Result<()> {
    let tmp = tmp_path(out_path);
    let digest = match stream_to_file(net, url, &tmp, decompress).await {
        Ok(digest) => digest,
        Err(err) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(err);
        }
    };
    // Case-insensitive: a manifest published with an uppercase digest is not tampered.
    if digest != sha256.to_lowercase() {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(AppError::new(
            ErrorCode::ChecksumMismatch,
            format!("update sha256 mismatch: want {sha256}, got {digest}"),
        ));
    }
    tokio::fs::rename(&tmp, out_path).await?;
    set_executable(out_path).await;
    Ok(())
}

fn tmp_path(out_path: &Path) -> PathBuf {
    let mut name = out_path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

/// `chmod 0755`, unix only. Best-effort: a placed-but-not-executable binary
/// surfaces at next launch, which the rollback anchor covers.
async fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(err) =
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).await
        {
            tracing::warn!(
                target: "loca::update",
                "could not mark {} executable: {err}",
                path.display(),
            );
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn corrupt_gzip(url: &str) -> impl Fn(std::io::Error) -> AppError + '_ {
    move |err| {
        AppError::new(
            ErrorCode::ChecksumMismatch,
            format!("{url} is not a readable gzip stream: {err}"),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::update::testing::{bounded, gzipped, sha256_hex, Serve, Upstream};

    /// Bigger than one chunk and compressible, so the decoder is driven across
    /// several writes.
    fn payload() -> Vec<u8> {
        b"#!/bin/sh\necho loca-core\n".repeat(4096)
    }

    /// Serve `body` and `download_and_verify` it (gzip) to `<tempdir>/<name>`.
    async fn fetch(
        body: Serve,
        sha256: &str,
        name: &str,
    ) -> (tempfile::TempDir, PathBuf, Result<()>) {
        let up = Upstream::start(body).await;
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join(name);
        let res = bounded(download_and_verify(
            &net,
            &up.url("/loca-core.gz"),
            &out,
            sha256,
            Decompress::Gzip,
        ))
        .await;
        (dir, out, res)
    }

    #[tokio::test]
    async fn the_digest_is_over_the_decompressed_artifact() {
        let plain = payload();
        let compressed = gzipped(&plain);
        let plain_digest = sha256_hex(&plain);
        assert_ne!(plain_digest, sha256_hex(&compressed));

        let (_d, out, res) = fetch(Serve::Body(compressed), &plain_digest, "loca-core").await;
        res.unwrap();
        assert_eq!(tokio::fs::read(&out).await.unwrap(), plain);
    }

    #[tokio::test]
    async fn an_uppercase_manifest_digest_still_matches() {
        let plain = payload();
        let digest = sha256_hex(&plain).to_uppercase();
        let (_d, out, res) = fetch(Serve::Body(gzipped(&plain)), &digest, "loca-core").await;
        res.unwrap();
        assert!(out.exists());
    }

    #[tokio::test]
    async fn a_mismatched_artifact_leaves_no_partial_behind() {
        let (_d, out, res) = fetch(
            Serve::Body(gzipped(&payload())),
            &"0".repeat(64),
            "loca-core",
        )
        .await;
        let err = res.unwrap_err();
        assert_eq!(err.code, ErrorCode::ChecksumMismatch);
        assert!(!out.exists(), "nothing is placed on a mismatch");
        assert!(!tmp_path(&out).exists(), "the refused partial is destroyed");
    }

    #[tokio::test]
    async fn a_transport_failure_leaves_no_partial_behind() {
        let (_d, out, res) = fetch(Serve::Status(500), &"0".repeat(64), "loca-core").await;
        res.unwrap_err();
        assert!(!out.exists());
        assert!(!tmp_path(&out).exists());
    }

    #[tokio::test]
    async fn a_body_that_is_not_gzip_fails_as_a_corrupt_archive() {
        let (_d, out, res) =
            fetch(Serve::Body(payload()), &sha256_hex(&payload()), "loca-core").await;
        let err = res.unwrap_err();
        assert!(err.message.contains("gzip"), "{}", err.message);
        assert!(!out.exists());
    }

    #[tokio::test]
    async fn a_truncated_archive_is_refused_rather_than_written_short() {
        let compressed = gzipped(&payload());
        let truncated = compressed[..compressed.len() / 2].to_vec();
        let (_d, out, res) =
            fetch(Serve::Body(truncated), &sha256_hex(&payload()), "loca-core").await;
        let err = res.unwrap_err();
        assert!(err.message.contains("gzip"), "{}", err.message);
        assert!(!out.exists());
    }

    #[tokio::test]
    async fn an_uncompressed_artifact_is_hashed_as_served() {
        let tarball = payload();
        let up = Upstream::start(Serve::Body(tarball.clone())).await;
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("builtin.tgz");

        let digest = bounded(stream_to_file(
            &net,
            &up.url("/builtin.tgz"),
            &out,
            Decompress::None,
        ))
        .await
        .unwrap();

        assert_eq!(digest, sha256_hex(&tarball));
        assert_eq!(tokio::fs::read(&out).await.unwrap(), tarball);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_placed_artifact_is_executable() {
        use std::os::unix::fs::PermissionsExt as _;
        let plain = payload();
        let (_d, out, res) = fetch(
            Serve::Body(gzipped(&plain)),
            &sha256_hex(&plain),
            "loca-core",
        )
        .await;
        res.unwrap();
        let mode = std::fs::metadata(&out).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "mode was {mode:o}");
    }

    #[tokio::test]
    async fn a_parent_directory_that_does_not_exist_yet_is_created() {
        let plain = payload();
        let (_d, out, res) = fetch(
            Serve::Body(gzipped(&plain)),
            &sha256_hex(&plain),
            "staging/deep/loca-core",
        )
        .await;
        res.unwrap();
        assert!(out.exists());
    }
}
