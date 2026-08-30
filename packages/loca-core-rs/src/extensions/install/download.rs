//! Acquiring an extension's files: verify, extract, and swap into place.
//! Nothing here runs extension code.
//!
//! The integrity gate runs BEFORE extraction and fails closed: plain sha256
//! (from the signed seeded manifest), then npm's SRI `sha512-`, then the
//! legacy `dist.shasum`, then refuse. A check that degraded to "unverified"
//! when registry metadata omits both fields would let a poisoned mirror serve
//! anything; npm always ships at least a shasum, so the refusal never blocks
//! a legitimate install.

use std::path::{Path, PathBuf};

use sha2::Digest as _;

use crate::contain::is_within;
use crate::error::{AppError, ErrorCode, Result};

/// What is known about a tarball's expected contents, by publisher.
#[derive(Debug, Clone, Default)]
pub struct Integrity {
    /// Plain hex sha256, from inside a signature.
    pub sha256: Option<String>,
    /// npm's SRI string, one or more space-separated `<alg>-<base64>` entries.
    pub integrity: Option<String>,
    /// npm's legacy hex sha1.
    pub shasum: Option<String>,
}

fn checksum_mismatch(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::ChecksumMismatch, message)
}

fn extract_failed(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::ExtractFailed, message)
}

/// Check `bytes` against whatever the publisher pinned, or refuse. Strongest
/// first, and each arm returns: a wrong sha256 is not rescued by a weaker hash.
pub fn verify_tarball(bytes: &[u8], source: &str, verify: &Integrity) -> Result<()> {
    if let Some(expected) = &verify.sha256 {
        let got = crate::files::sha256_hex(bytes);
        if got != expected.to_lowercase() {
            return Err(checksum_mismatch(format!(
                "tarball failed sha256 check for {source}"
            )));
        }
        return Ok(());
    }
    if let Some(expected) = sri_sha512(verify.integrity.as_deref()) {
        use base64::prelude::{Engine as _, BASE64_STANDARD};
        let got = BASE64_STANDARD.encode(sha2::Sha512::digest(bytes));
        if got != expected {
            return Err(checksum_mismatch(format!(
                "npm tarball failed sha512 integrity check for {source}"
            )));
        }
        return Ok(());
    }
    if let Some(expected) = verify.shasum.as_deref() {
        // Legacy SHA-1: the only integrity npm offers for old packages, and
        // checking it is strictly better than not.
        use sha1::Digest as _;
        let got = hex::encode(sha1::Sha1::digest(bytes));
        if !got.eq_ignore_ascii_case(expected) {
            return Err(checksum_mismatch(format!(
                "npm tarball failed sha1 integrity check for {source}"
            )));
        }
        return Ok(());
    }
    Err(checksum_mismatch(format!(
        "npm tarball {source} has no integrity/shasum metadata; refusing to install unverified"
    )))
}

/// The `sha512-` entry of a (possibly multi-hash) SRI string, if there is one.
fn sri_sha512(integrity: Option<&str>) -> Option<String> {
    integrity?
        .split_whitespace()
        .find_map(|entry| entry.strip_prefix("sha512-"))
        .map(str::to_string)
}

/// What a tar entry may become on disk: an allowlist, since a symlink, hardlink
/// or device node could escape the staging directory on a later write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
}

/// ustar typeflag classification.
pub fn extractable_entry_type(typeflag: u8) -> Option<EntryKind> {
    match typeflag {
        b'5' => Some(EntryKind::Dir),
        b'0' | 0 => Some(EntryKind::File),
        _ => None,
    }
}

/// npm tarballs always begin with `package/`. Strip it, and answer `None` for
/// an entry that is nothing but the prefix.
pub fn strip_package_prefix(path: &str) -> Option<&str> {
    let stripped = path.strip_prefix("package/").unwrap_or(path);
    (!stripped.is_empty()).then_some(stripped)
}

/// Gunzip and untar ALREADY-VERIFIED bytes into `target_dir`; callers must have
/// run [`verify_tarball`] over the same buffer.
pub fn extract_tarball(bytes: &[u8], target_dir: &Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    std::fs::create_dir_all(target_dir)?;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw = entry.path()?.to_string_lossy().into_owned();
        let Some(stripped) = strip_package_prefix(&raw) else {
            continue;
        };

        let Some(kind) = extractable_entry_type(entry.header().entry_type().as_byte()) else {
            return Err(extract_failed(format!(
                "tarball entry \"{raw}\" has unsupported type {} \
                 (symlinks/hardlinks/devices are not allowed)",
                entry.header().entry_type().as_byte(),
            )));
        };

        let out = target_dir.join(stripped);
        // Zip-slip guard.
        if !is_within(target_dir, &out) {
            return Err(extract_failed(format!(
                "tarball entry \"{raw}\" escapes the extension directory"
            )));
        }
        match kind {
            EntryKind::Dir => std::fs::create_dir_all(&out)?,
            EntryKind::File => {
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut file = std::fs::File::create(&out)?;
                std::io::copy(&mut entry, &mut file)?;
            }
        }
    }
    Ok(())
}

/// Move a staged folder into its final place: `<id>` to `<id>.old`, then
/// `<id>.new` to `<id>`, restoring the old one if the second rename fails.
pub fn swap_into_place(staging: &Path, install: &Path, extension_id: &str) -> Result<()> {
    let old = with_suffix(install, ".old");
    rm_rf(&old)?;

    let had_old = install.exists();
    if had_old {
        std::fs::rename(install, &old)?;
    }
    if let Err(err) = std::fs::rename(staging, install) {
        if had_old {
            if let Err(rollback) = std::fs::rename(&old, install) {
                // The only record of where the stranded files went.
                tracing::error!(
                    target: "loca::extensions",
                    "{extension_id}: rollback rename failed; previous version is at {}: {rollback}",
                    old.display(),
                );
            }
        }
        return Err(err.into());
    }
    rm_rf(&old)?;
    Ok(())
}

/// `path` with `suffix` appended to the whole name (`Path::with_extension`
/// would turn `@scope__pkg.io` into `@scope__pkg.old`).
pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Recursive delete that treats "already gone" as success.
pub fn rm_rf(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        // A user can put a plain file where a directory is expected.
        Err(_) => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        },
    }
}

/// Copy a source tree for a `local` install, skipping `node_modules` at every
/// depth (the install phase rebuilds it and the hash excludes it).
pub fn copy_tree_excluding_node_modules(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if entry.file_name() == "node_modules" {
            continue;
        }
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree_excluding_node_modules(&from, &to)?;
        } else if kind.is_file() {
            std::fs::copy(&from, &to)?;
        }
        // A symlink is neither copied nor followed: the content hash refuses one.
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use base64::prelude::{Engine as _, BASE64_STANDARD};

    fn gz_tar(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, kind, body) in entries {
            let mut header = tar::Header::new_ustar();
            header.set_size(body.len() as u64);
            header.set_entry_type(*kind);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *body).unwrap();
        }
        gzip(&builder.into_inner().unwrap())
    }

    /// A tar with a hand-written header: the `tar` builder refuses `..` paths.
    fn hostile_tar(name: &str, body: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        // mode, uid, gid: octal, NUL-terminated.
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        let size = format!("{:011o}\0", body.len());
        header[124..136].copy_from_slice(size.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        "); // checksum placeholder
        header[156] = b'0'; // regular file
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
        let checksum = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());

        let mut out = header.to_vec();
        out.extend_from_slice(body);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out.extend_from_slice(&[0u8; 1024]); // two empty blocks end the archive
        gzip(&out)
    }

    fn sha256(v: impl Into<String>) -> Integrity {
        Integrity {
            sha256: Some(v.into()),
            ..Integrity::default()
        }
    }

    fn sri(v: impl Into<String>) -> Integrity {
        Integrity {
            integrity: Some(v.into()),
            ..Integrity::default()
        }
    }

    fn shasum(v: impl Into<String>) -> Integrity {
        Integrity {
            shasum: Some(v.into()),
            ..Integrity::default()
        }
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, bytes).unwrap();
        gz.finish().unwrap()
    }

    // --- the integrity gate ---------------------------------------------------

    #[test]
    fn a_tarball_with_no_integrity_metadata_at_all_is_refused() {
        let err = verify_tarball(b"anything", "u", &Integrity::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ChecksumMismatch);
        assert!(err.message.contains("refusing to install unverified"));
    }

    #[test]
    fn the_sha256_pin_is_checked_and_a_wrong_one_is_not_rescued_by_a_weaker_hash() {
        let bytes = b"the tarball bytes";
        let digest = crate::files::sha256_hex(bytes);

        verify_tarball(bytes, "u", &sha256(digest.to_uppercase()))
            .expect("an uppercase hex pin must still match");

        let err = verify_tarball(
            bytes,
            "u",
            &Integrity {
                integrity: Some(format!(
                    "sha512-{}",
                    BASE64_STANDARD.encode(sha2::Sha512::digest(bytes))
                )),
                ..sha256("b".repeat(64))
            },
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ChecksumMismatch);
    }

    #[test]
    fn the_sri_entry_that_is_checked_is_the_sha512_one() {
        let bytes = b"npm bytes";
        let sha512 = BASE64_STANDARD.encode(sha2::Sha512::digest(bytes));
        let sha1 = "0".repeat(40);

        verify_tarball(bytes, "u", &sri(format!("sha1-{sha1} sha512-{sha512}")))
            .expect("the sha512 entry was not found in a multi-entry SRI string");

        let bad = sri(format!("sha512-{}", BASE64_STANDARD.encode([0u8; 64])));
        let err = verify_tarball(bytes, "u", &bad).unwrap_err();
        assert_eq!(err.code, ErrorCode::ChecksumMismatch);

        // Weak-only SRI falls through to the shasum arm, and with none, refuses.
        let err = verify_tarball(bytes, "u", &sri(format!("sha1-{sha1}"))).unwrap_err();
        assert!(err.message.contains("refusing to install unverified"));
    }

    #[test]
    fn a_legacy_sha1_shasum_is_verified_in_both_directions() {
        let bytes = b"legacy bytes";
        let good = {
            use sha1::Digest as _;
            hex::encode(sha1::Sha1::digest(bytes))
        };
        verify_tarball(bytes, "u", &shasum(good.clone())).expect("a matching sha1 installs");

        verify_tarball(bytes, "u", &shasum(good.to_uppercase()))
            .expect("sha1 comparison is case-insensitive");

        let err = verify_tarball(bytes, "u", &shasum("0".repeat(40))).unwrap_err();
        assert_eq!(err.code, ErrorCode::ChecksumMismatch);
        assert!(err.message.contains("sha1"), "{}", err.message);
    }

    // --- extraction -----------------------------------------------------------

    #[test]
    fn only_plain_files_and_directories_are_extractable() {
        assert_eq!(extractable_entry_type(b'0'), Some(EntryKind::File));
        assert_eq!(extractable_entry_type(0), Some(EntryKind::File));
        assert_eq!(extractable_entry_type(b'5'), Some(EntryKind::Dir));
        // Symlink, hardlink, char/block device, fifo, GNU long name.
        for flag in [b'1', b'2', b'3', b'4', b'6', b'L', b'K'] {
            assert_eq!(extractable_entry_type(flag), None, "typeflag {flag}");
        }
    }

    #[test]
    fn the_package_prefix_is_stripped_and_a_bare_prefix_entry_is_skipped() {
        assert_eq!(strip_package_prefix("package/main.ts"), Some("main.ts"));
        assert_eq!(strip_package_prefix("package/"), None);
        assert_eq!(strip_package_prefix("main.ts"), Some("main.ts"));
        assert_eq!(strip_package_prefix(""), None);
    }

    #[test]
    fn a_tarball_entry_cannot_escape_the_install_dir() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("staging");
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, b"original").unwrap();

        let bytes = hostile_tar("package/../outside.txt", b"overwritten");
        let err = extract_tarball(&bytes, &target).unwrap_err();
        assert_eq!(err.code, ErrorCode::ExtractFailed);
        assert!(err.message.contains("escapes"), "{}", err.message);
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            b"original",
            "the escaping entry was written anyway"
        );
    }

    #[test]
    fn an_absolute_tarball_entry_cannot_write_outside_the_install_dir_either() {
        // `Path::join` replaces the base for an absolute argument.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("staging");
        let outside = dir.path().join("absolute.txt");
        std::fs::write(&outside, b"original").unwrap();

        // No `package/` prefix, or it would strip to a harmless relative path.
        let bytes = hostile_tar(&outside.to_string_lossy(), b"overwritten");
        let err = extract_tarball(&bytes, &target).unwrap_err();
        assert_eq!(err.code, ErrorCode::ExtractFailed);
        assert_eq!(std::fs::read(&outside).unwrap(), b"original");
    }

    #[test]
    fn a_symlink_entry_is_refused_rather_than_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_ustar();
        header.set_size(0);
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        builder
            .append_link(&mut header, "package/link", "/etc/passwd")
            .unwrap();
        let bytes = gzip(&builder.into_inner().unwrap());

        let err = extract_tarball(&bytes, &dir.path().join("staging")).unwrap_err();
        assert_eq!(err.code, ErrorCode::ExtractFailed);
        assert!(err.message.contains("symlinks"), "{}", err.message);
    }

    #[test]
    fn an_ordinary_tarball_extracts_with_its_package_prefix_removed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("staging");
        let bytes = gz_tar(&[
            ("package/loca.json", tar::EntryType::Regular, b"{}"),
            ("package/src/main.ts", tar::EntryType::Regular, b"export {}"),
            ("package/empty/", tar::EntryType::Directory, b""),
        ]);
        extract_tarball(&bytes, &target).unwrap();
        assert_eq!(std::fs::read(target.join("loca.json")).unwrap(), b"{}");
        assert_eq!(
            std::fs::read(target.join("src/main.ts")).unwrap(),
            b"export {}"
        );
        assert!(target.join("empty").is_dir());
        assert!(!target.join("package").exists(), "the prefix survived");
    }

    // --- the swap -------------------------------------------------------------

    #[test]
    fn a_swap_replaces_the_previous_version_and_leaves_no_old_folder() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("ext");
        let staging = dir.path().join("ext.new");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("v"), b"1").unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("v"), b"2").unwrap();

        swap_into_place(&staging, &install, "ext").unwrap();
        assert_eq!(std::fs::read(install.join("v")).unwrap(), b"2");
        assert!(!staging.exists());
        assert!(
            !dir.path().join("ext.old").exists(),
            "the previous version was left on disk forever"
        );
    }

    #[test]
    fn a_fresh_install_has_no_previous_version_to_move() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("ext");
        let staging = dir.path().join("ext.new");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("v"), b"1").unwrap();
        swap_into_place(&staging, &install, "ext").unwrap();
        assert_eq!(std::fs::read(install.join("v")).unwrap(), b"1");
    }

    #[test]
    fn a_failed_swap_puts_the_previous_version_back() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("ext");
        let staging = dir.path().join("ext.new");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("v"), b"1").unwrap();
        // No staging directory at all, so the second rename fails.
        assert!(swap_into_place(&staging, &install, "ext").is_err());
        assert_eq!(
            std::fs::read(install.join("v")).unwrap(),
            b"1",
            "the previous version was not restored"
        );
        assert!(!dir.path().join("ext.old").exists());
    }

    #[test]
    fn a_stale_old_folder_from_a_crashed_run_is_cleared_first() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("ext");
        let staging = dir.path().join("ext.new");
        let stale = dir.path().join("ext.old");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("junk"), b"from a crash").unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("v"), b"2").unwrap();

        swap_into_place(&staging, &install, "ext").unwrap();
        assert_eq!(std::fs::read(install.join("v")).unwrap(), b"2");
        assert!(!stale.exists());
    }

    #[test]
    fn the_staging_suffix_is_appended_to_the_whole_name() {
        assert_eq!(
            with_suffix(Path::new("/x/@scope__pkg.io"), ".new"),
            PathBuf::from("/x/@scope__pkg.io.new")
        );
        assert_eq!(
            with_suffix(Path::new("/x/plain"), ".old"),
            PathBuf::from("/x/plain.old")
        );
    }

    // --- the local copy -------------------------------------------------------

    #[test]
    fn a_local_copy_takes_the_tree_but_never_node_modules() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::create_dir_all(src.join("nested/node_modules/dep")).unwrap();
        std::fs::create_dir_all(src.join("node_modules/dep")).unwrap();
        std::fs::write(src.join("loca.json"), b"{}").unwrap();
        std::fs::write(src.join("nested/main.ts"), b"x").unwrap();
        std::fs::write(src.join("node_modules/dep/i.js"), b"x").unwrap();
        std::fs::write(src.join("nested/node_modules/dep/i.js"), b"x").unwrap();

        copy_tree_excluding_node_modules(&src, &dst).unwrap();
        assert!(dst.join("loca.json").exists());
        assert!(dst.join("nested/main.ts").exists());
        assert!(!dst.join("node_modules").exists());
        assert!(
            !dst.join("nested/node_modules").exists(),
            "node_modules was only skipped at the root"
        );
    }

    #[test]
    fn a_local_copy_does_not_follow_a_symlink_out_of_the_source_tree() {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("src");
            let dst = dir.path().join("dst");
            let secret = dir.path().join("secret.txt");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(&secret, b"not yours").unwrap();
            std::os::unix::fs::symlink(&secret, src.join("link.txt")).unwrap();
            std::fs::write(src.join("loca.json"), b"{}").unwrap();

            copy_tree_excluding_node_modules(&src, &dst).unwrap();
            assert!(dst.join("loca.json").exists());
            assert!(
                !dst.join("link.txt").exists(),
                "a symlink was followed out of the source tree"
            );
        }
    }

    #[test]
    fn removing_something_that_is_not_there_is_success() {
        let dir = tempfile::tempdir().unwrap();
        rm_rf(&dir.path().join("never-existed")).unwrap();
        let file = dir.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        rm_rf(&file).unwrap();
        assert!(!file.exists());
    }
}
