//! Download an artifact, decompress it, hash it as it streams, and place it.
//!
//! `downloadAndVerify` (`self-updater.ts:507-549`), which the fresh-install
//! path deliberately shares (`install/fetch-verify.ts:1-8`) so a first install
//! and an update run one audited trust path rather than two.
//!
//! # The digest is over the DECOMPRESSED bytes
//!
//! Core artifacts ship gzip-compressed (`.gz` URLs) and the manifest's `sha256`
//! is over the file that comes OUT (`scripts/release/core.ts:606-609`). So the
//! decompression happens before the hash, not after the verify, and hashing the
//! bytes off the wire would fail every artifact on every release. It is a
//! one-line ordering with no local symptom, which is why it has a test of its
//! own rather than being covered incidentally by the happy path.
//!
//! The built-in extension tarball is the exception and hashes as-is
//! (`bootstrap.ts:153-159`), which is why [`Decompress`] is a parameter rather
//! than a constant.
//!
//! # Nothing is held in memory
//!
//! The core binary is around 100 MB. The decoder is drained per chunk and the
//! digest is incremental, so peak memory is one chunk plus whatever one chunk
//! inflates to, not the artifact.
//!
//! # The fixed `.tmp`
//!
//! `<out>.tmp`, matching the oracle, and not the unique suffix `files::write_*`
//! uses. The reason that rule exists is concurrent writers racing their renames
//! into a spurious `ENOENT`, and there are none here: an apply clears the whole
//! staging directory before it writes anything (`self-updater.ts:131`), the
//! names inside it carry the version, and only one apply can be in flight
//! because the first one exits the process. A named temp is also what lets a
//! failed update leave something a human can look at.

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
/// written.
///
/// The caller compares. Split out because the two callers differ: the update
/// path verifies against a manifest digest, and the extension plant compares
/// against a manifest it has already read and then renames the file itself.
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
    // `write::GzDecoder` over a `Vec` rather than a reader over the stream: the
    // body arrives as awaited chunks, so the data is pushed in rather than
    // pulled, and the buffer is drained after every chunk so it never grows to
    // the size of the artifact.
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
        // Without this the last deflate block and the gzip trailer check are
        // both skipped, so a truncated archive would be written out short and
        // then reported only as a digest mismatch, which names the wrong cause.
        decoder.try_finish().map_err(corrupt_gzip(url))?;
        let inflated = std::mem::take(decoder.get_mut());
        if !inflated.is_empty() {
            hasher.update(&inflated);
            file.write_all(&inflated).await?;
        }
    }

    file.flush().await?;
    // Closed before the digest is compared, so the caller's rename cannot race
    // an unflushed handle on Windows.
    drop(file);
    Ok(hex::encode(hasher.finalize()))
}

/// Download `url` to `out_path` and verify it against `sha256`.
///
/// On a mismatch the partial is destroyed and nothing is placed. Leaving it
/// would put a file at a staging path that a later step reads by name, so the
/// next attempt could resume onto bytes that already failed verification.
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
            // A transport failure leaves no partial either, for the same
            // reason. Best-effort: the download already failed and a failure to
            // clean up must not replace its message with a less useful one.
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(err);
        }
    };
    // Case-insensitively, matching `sha256.toLowerCase()` at
    // `self-updater.ts:537`. A manifest published with an uppercase digest is
    // otherwise rejected as tampered, which is unfalsifiable from a support log.
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

/// `chmod 0755`, on unix only (`self-updater.ts:546-548`).
///
/// Best-effort by design: every artifact this places is executable or is a
/// script the core spawns, and a placement that succeeded but could not be
/// chmod-ed is still better than a failed update. A non-executable core binary
/// surfaces at the next launch, which the rollback anchor covers.
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

    fn payload() -> Vec<u8> {
        // Bigger than one chunk of anything, and compressible, so the decoder
        // is genuinely driven across several writes rather than satisfied by
        // one.
        b"#!/bin/sh\necho loca-core\n".repeat(4096)
    }

    #[tokio::test]
    async fn the_digest_is_over_the_decompressed_artifact() {
        // The ordering with no local symptom: hashing the bytes off the wire
        // compiles, runs, writes the right file, and fails every artifact on
        // every release. The fixture's two digests differ, which is what makes
        // this test able to tell them apart.
        let plain = payload();
        let compressed = gzipped(&plain);
        let plain_digest = sha256_hex(&plain);
        let wire_digest = sha256_hex(&compressed);
        assert_ne!(plain_digest, wire_digest);

        let up = Upstream::start(Serve::Body(compressed)).await;
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("loca-core");

        bounded(download_and_verify(
            &net,
            &up.url("/loca-core.gz"),
            &out,
            &plain_digest,
            Decompress::Gzip,
        ))
        .await
        .unwrap();

        assert_eq!(tokio::fs::read(&out).await.unwrap(), plain);
    }

    #[tokio::test]
    async fn an_uppercase_manifest_digest_still_matches() {
        let plain = payload();
        let up = Upstream::start(Serve::Body(gzipped(&plain))).await;
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("loca-core");

        bounded(download_and_verify(
            &net,
            &up.url("/loca-core.gz"),
            &out,
            &sha256_hex(&plain).to_uppercase(),
            Decompress::Gzip,
        ))
        .await
        .unwrap();
        assert!(out.exists());
    }

    #[tokio::test]
    async fn a_mismatched_artifact_leaves_no_partial_behind() {
        let up = Upstream::start(Serve::Body(gzipped(&payload()))).await;
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("loca-core");

        let err = bounded(download_and_verify(
            &net,
            &up.url("/loca-core.gz"),
            &out,
            &"0".repeat(64),
            Decompress::Gzip,
        ))
        .await
        .unwrap_err();

        assert_eq!(err.code, ErrorCode::ChecksumMismatch);
        assert!(!out.exists(), "nothing is placed on a mismatch");
        assert!(
            !tmp_path(&out).exists(),
            "and the partial that failed verification is destroyed, so a later \
             step cannot read bytes that were already refused"
        );
    }

    #[tokio::test]
    async fn a_transport_failure_leaves_no_partial_behind() {
        let up = Upstream::start(Serve::Status(500)).await;
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("loca-core");

        bounded(download_and_verify(
            &net,
            &up.url("/loca-core.gz"),
            &out,
            &"0".repeat(64),
            Decompress::Gzip,
        ))
        .await
        .unwrap_err();

        assert!(!out.exists());
        assert!(!tmp_path(&out).exists());
    }

    #[tokio::test]
    async fn a_body_that_is_not_gzip_fails_as_a_corrupt_archive() {
        // Named rather than surfacing as a digest mismatch, because the two
        // have different causes and different fixes: one is a broken mirror,
        // the other is a manifest that disagrees with its artifact.
        let up = Upstream::start(Serve::Body(payload())).await;
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("loca-core");

        let err = bounded(download_and_verify(
            &net,
            &up.url("/loca-core.gz"),
            &out,
            &sha256_hex(&payload()),
            Decompress::Gzip,
        ))
        .await
        .unwrap_err();
        assert!(err.message.contains("gzip"), "{}", err.message);
        assert!(!out.exists());
    }

    #[tokio::test]
    async fn a_truncated_archive_is_refused_rather_than_written_short() {
        // Without the finish, a body cut mid-stream writes whatever inflated so
        // far and reports only a digest mismatch, which names the wrong cause.
        let compressed = gzipped(&payload());
        let truncated = compressed[..compressed.len() / 2].to_vec();
        let up = Upstream::start(Serve::Body(truncated)).await;
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("loca-core");

        let err = bounded(download_and_verify(
            &net,
            &up.url("/loca-core.gz"),
            &out,
            &sha256_hex(&payload()),
            Decompress::Gzip,
        ))
        .await
        .unwrap_err();
        assert!(err.message.contains("gzip"), "{}", err.message);
        assert!(!out.exists());
    }

    #[tokio::test]
    async fn an_uncompressed_artifact_is_hashed_as_served() {
        // The built-in extension tarball's path: its published digest is over
        // the file as served, so running it through the decoder would fail
        // every plant.
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
        let up = Upstream::start(Serve::Body(gzipped(&plain))).await;
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("loca-core");

        bounded(download_and_verify(
            &net,
            &up.url("/loca-core.gz"),
            &out,
            &sha256_hex(&plain),
            Decompress::Gzip,
        ))
        .await
        .unwrap();

        let mode = std::fs::metadata(&out).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "mode was {mode:o}");
    }

    #[tokio::test]
    async fn a_parent_directory_that_does_not_exist_yet_is_created() {
        // The staging directory is removed and recreated per apply, and the
        // helper destinations may be a bin dir a fresh install has not made.
        let plain = payload();
        let up = Upstream::start(Serve::Body(gzipped(&plain))).await;
        let net = Net::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("staging").join("deep").join("loca-core");

        bounded(download_and_verify(
            &net,
            &up.url("/loca-core.gz"),
            &out,
            &sha256_hex(&plain),
            Decompress::Gzip,
        ))
        .await
        .unwrap();
        assert!(out.exists());
    }
}
