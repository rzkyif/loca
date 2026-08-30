//! The bundled espeak-ng phonemizer data, and getting it onto disk. kokoro,
//! kitten, vits and matcha all phonemize through espeak-ng, and multilingual
//! Kokoro terminates the process without it, so this is a required runtime
//! resource.
//!
//! Embedded rather than downloaded: fetching a required resource after the user
//! enabled speech would be a background network call, and at 8.6 MB compressed
//! embedding is also the cheap option and the only one that behaves the same on
//! desktop and mobile. Only linked in under `local-speech`.
//!
//! Extraction is idempotent and self-healing: the tree is written once to
//! `~/.loca/<channel>/espeak-ng-data` with a stamp recording the archive's
//! sha256, and a missing or different stamp rewrites it, so a half-extracted
//! directory recovers on the next synthesis.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{internal, Result};

/// The stamp that records which archive the tree on disk came from.
const STAMP: &str = ".loca-espeak-sha256";

/// The files sherpa itself checks before loading a phonemizer, so their absence
/// must invalidate the tree. Deliberately the SAME four rather than a token
/// file of our own: checking fewer means handing sherpa a tree it will reject,
/// which surfaces as "the voice model could not be loaded".
const REQUIRED_FILES: &[&str] = &["phondata", "phontab", "phonindex", "intonations"];

/// Where the phonemizer data lives, under the channel root so the storage
/// screen can see it and a channel switch does not share it.
pub fn data_dir(root: &Path) -> PathBuf {
    root.join("espeak-ng-data")
}

/// Ensure `data_dir(root)` holds the tree packed in `archive`, and answer where
/// it is. `archive` is a gzipped tar rooted at `espeak-ng-data/`. Extraction
/// goes to a sibling temporary directory and is renamed into place, and the
/// stamp is written last, so a crash costs one redundant extraction rather than
/// leaving a tree that claims to be complete.
pub fn ensure(root: &Path, archive: &[u8]) -> Result<PathBuf> {
    let dir = data_dir(root);
    let digest = hex_sha256(archive);
    if is_current(&dir, &digest) {
        return Ok(dir);
    }

    let staging = dir.with_extension("unpacking");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|err| internal(format!("could not clear {}: {err}", staging.display())))?;
    }
    std::fs::create_dir_all(&staging)
        .map_err(|err| internal(format!("could not create {}: {err}", staging.display())))?;

    unpack(archive, &staging)?;

    // The archive is rooted at espeak-ng-data/, so the tree we want is one
    // level in.
    let unpacked = staging.join("espeak-ng-data");
    let source = if unpacked.is_dir() {
        unpacked
    } else {
        staging.clone()
    };

    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|err| internal(format!("could not replace {}: {err}", dir.display())))?;
    }
    std::fs::rename(&source, &dir).map_err(|err| {
        internal(format!(
            "could not move the phonemizer data into place: {err}"
        ))
    })?;
    let _ = std::fs::remove_dir_all(&staging);

    std::fs::write(dir.join(STAMP), &digest)
        .map_err(|err| internal(format!("could not stamp {}: {err}", dir.display())))?;
    tracing::info!(
        target: "loca::tts",
        "extracted the espeak-ng phonemizer data to {}", dir.display(),
    );
    Ok(dir)
}

/// True when the tree on disk came from this exact archive AND still has its
/// dictionaries. The content check is what makes this self-healing: a stamp
/// alone would call a tree current after someone deleted its contents.
fn is_current(dir: &Path, digest: &str) -> bool {
    if REQUIRED_FILES.iter().any(|name| !dir.join(name).is_file()) {
        return false;
    }
    std::fs::read_to_string(dir.join(STAMP)).is_ok_and(|stamped| stamped.trim() == digest)
}

fn unpack(archive: &[u8], into: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    // `unpack_in` refuses absolute paths and `..` traversal. The archive is
    // ours and pinned by sha256, so this is defence in depth.
    for entry in tar
        .entries()
        .map_err(|err| internal(format!("could not read the phonemizer archive: {err}")))?
    {
        let mut entry =
            entry.map_err(|err| internal(format!("could not read an archive entry: {err}")))?;
        let unpacked = entry
            .unpack_in(into)
            .map_err(|err| internal(format!("could not unpack the phonemizer data: {err}")))?;
        if !unpacked {
            return Err(internal(
                "the phonemizer archive contains an entry that escapes its directory",
            ));
        }
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A gzipped tar shaped like the real one: rooted at `espeak-ng-data/` and
    /// carrying a `phondata`. Built here rather than committed so the test does
    /// not depend on the 8.6 MB archive being present.
    fn archive(marker: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(marker.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header.clone(), "espeak-ng-data/phondata", marker)
            .unwrap();
        // The other files `is_current` requires, plus a dictionary, so the
        // fixture is a tree this module would actually call current.
        for name in ["phontab", "phonindex", "intonations", "en_dict"] {
            let mut entry = tar::Header::new_gnu();
            entry.set_size(3);
            entry.set_mode(0o644);
            entry.set_cksum();
            builder
                .append_data(&mut entry, format!("espeak-ng-data/{name}"), &b"abc"[..])
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn the_tree_lands_under_the_channel_root() {
        let root = tempfile::tempdir().unwrap();
        let dir = ensure(root.path(), &archive(b"one")).unwrap();

        assert_eq!(dir, root.path().join("espeak-ng-data"));
        assert_eq!(std::fs::read(dir.join("phondata")).unwrap(), b"one");
        assert!(dir.join("en_dict").is_file());
        // No nested espeak-ng-data/espeak-ng-data: espeak would find no
        // dictionaries and the model load would take the process down.
        assert!(!dir.join("espeak-ng-data").exists());
        // The staging directory does not survive. Derived from `data_dir`
        // rather than spelled out, so a wrong literal cannot make it vacuous.
        assert!(!data_dir(root.path()).with_extension("unpacking").exists());
    }

    #[test]
    fn a_second_call_with_the_same_archive_does_not_rewrite_the_tree() {
        let root = tempfile::tempdir().unwrap();
        let bytes = archive(b"one");
        let dir = ensure(root.path(), &bytes).unwrap();

        // A file the archive does not contain survives only if extraction was
        // genuinely skipped, which is the property "idempotent" has to mean
        // here: a rewrite would delete the whole tree first.
        std::fs::write(dir.join("witness"), b"still here").unwrap();
        ensure(root.path(), &bytes).unwrap();
        assert!(dir.join("witness").is_file(), "the tree was rewritten");
    }

    /// Every file sherpa validates must invalidate the tree when it goes
    /// missing, not just the first one named. A narrower `REQUIRED_FILES` is
    /// indistinguishable until a tree is partly deleted.
    #[test]
    fn removing_any_file_sherpa_checks_invalidates_the_tree() {
        // Spelled out, NOT read from `REQUIRED_FILES`: a loop over the
        // constant it guards shrinks with it and keeps passing. These four are
        // what sherpa validates.
        const SHERPA_CHECKS: &[&str] = &["phondata", "phontab", "phonindex", "intonations"];
        let bytes = archive(b"one");
        for missing in SHERPA_CHECKS {
            let root = tempfile::tempdir().unwrap();
            let dir = ensure(root.path(), &bytes).unwrap();
            std::fs::remove_file(dir.join(missing)).unwrap();

            // A witness the archive does not contain: it survives only if
            // extraction was skipped, which is what "the tree was trusted"
            // means.
            std::fs::write(dir.join("witness"), b"still here").unwrap();
            ensure(root.path(), &bytes).unwrap();

            assert!(
                dir.join(missing).is_file(),
                "a tree missing {missing} was not rebuilt",
            );
            assert!(
                !dir.join("witness").exists(),
                "a tree missing {missing} was trusted rather than re-extracted",
            );
        }
    }

    #[test]
    fn a_new_archive_replaces_the_old_tree() {
        let root = tempfile::tempdir().unwrap();
        let dir = ensure(root.path(), &archive(b"one")).unwrap();
        std::fs::write(dir.join("witness"), b"from the old tree").unwrap();

        ensure(root.path(), &archive(b"two")).unwrap();
        assert_eq!(std::fs::read(dir.join("phondata")).unwrap(), b"two");
        assert!(
            !dir.join("witness").exists(),
            "the old tree survived a version change",
        );
    }

    #[test]
    fn a_gutted_tree_is_rebuilt_rather_than_trusted() {
        // Self-healing: a stamp on its own would call this tree current, and
        // the next synthesis would terminate the process.
        let root = tempfile::tempdir().unwrap();
        let bytes = archive(b"one");
        let dir = ensure(root.path(), &bytes).unwrap();

        std::fs::remove_file(dir.join("phondata")).unwrap();
        ensure(root.path(), &bytes).unwrap();
        assert_eq!(std::fs::read(dir.join("phondata")).unwrap(), b"one");
    }

    #[test]
    fn a_stale_staging_directory_does_not_block_extraction() {
        // A crash mid-extraction leaves one behind; the next call must clear it
        // rather than fail forever.
        let root = tempfile::tempdir().unwrap();
        let staging = data_dir(root.path()).with_extension("unpacking");
        std::fs::create_dir_all(staging.join("junk")).unwrap();

        let dir = ensure(root.path(), &archive(b"one")).unwrap();
        assert!(dir.join("phondata").is_file());
        assert!(!staging.exists());
    }

    /// A tar entry whose name escapes the extraction directory. Hand-built,
    /// because `tar`'s `append_data` refuses to WRITE a path containing `..`,
    /// and that refusal is a property of the writer rather than the reader.
    fn escaping_archive() -> Vec<u8> {
        const NAME: &[u8] = b"../escaped";
        const BODY: &[u8] = b"bad";
        let mut header = [0u8; 512];
        header[..NAME.len()].copy_from_slice(NAME);
        header[100..107].copy_from_slice(b"0000644"); // mode
        header[108..115].copy_from_slice(b"0000000"); // uid
        header[116..123].copy_from_slice(b"0000000"); // gid
        header[124..135].copy_from_slice(b"00000000003"); // size, octal
        header[136..147].copy_from_slice(b"00000000000"); // mtime
        header[156] = b'0'; // typeflag: regular file
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        // The checksum is computed with its own field read as spaces.
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
        let checksum = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());

        let mut tar_bytes = Vec::new();
        tar_bytes.extend_from_slice(&header);
        let mut block = [0u8; 512];
        block[..BODY.len()].copy_from_slice(BODY);
        tar_bytes.extend_from_slice(&block);
        tar_bytes.extend_from_slice(&[0u8; 1024]); // end of archive

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &tar_bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn an_archive_that_escapes_its_directory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().parent().unwrap().join("escaped");
        let _ = std::fs::remove_file(&outside);

        assert!(ensure(root.path(), &escaping_archive()).is_err());
        assert!(!outside.exists(), "the entry escaped its directory");
        assert!(!data_dir(root.path()).join("phondata").exists());
    }

    #[test]
    fn a_corrupt_archive_is_an_error_rather_than_a_partial_tree() {
        let root = tempfile::tempdir().unwrap();
        let err = ensure(root.path(), b"this is not a gzip stream").unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::InternalError);
        assert!(!data_dir(root.path()).join("phondata").exists());
    }
}
