//! Acquiring an extension's files: verify, extract, and swap into place.
//!
//! Ports the byte-handling half of
//! `packages/loca-core/src/extensions/installer-download.ts`.
//!
//! Nothing here runs extension code. The manifest is parsed, the folder is
//! swapped, and the rows are written; `deno install` is the separate phase in
//! [`super::deps`], and a worker is later still.
//!
//! # The integrity gate runs BEFORE extraction and fails closed
//!
//! The whole body is buffered, checked, and only then untarred. The precedence
//! is plain sha256 (what the signed seeded manifest pins), then npm's SRI
//! `sha512-`, then npm's legacy `dist.shasum`, then **refuse**. There is no
//! path that installs an unverified tarball, and the refusal is the point: a
//! supply-chain check that silently degrades to "no verification" when the
//! registry metadata omits both fields lets a poisoned mirror serve anything it
//! likes. npm always ships at least a shasum, so the refusal never blocks a
//! legitimate install.

use std::path::{Path, PathBuf};

use sha2::Digest as _;

use crate::contain::is_within;
use crate::error::{AppError, ErrorCode, Result};

/// What is known about a tarball's expected contents.
///
/// All three are `Option` because they come from different publishers: the
/// signed seeded manifest pins `sha256` directly, and npm supplies the other
/// two in its (untrusted) package metadata.
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

/// Check `bytes` against whatever the publisher pinned, or refuse.
///
/// The order is strongest-first and each arm RETURNS, so a present-but-wrong
/// sha256 is a failure rather than something a weaker hash could rescue.
pub fn verify_tarball(bytes: &[u8], source: &str, verify: &Integrity) -> Result<()> {
    if let Some(expected) = &verify.sha256 {
        let got = hex::encode(sha2::Sha256::digest(bytes));
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
        // The legacy arm: an npm package old enough to publish `dist.shasum`
        // (hex SHA-1) and no `dist.integrity`. It used to REFUSE, because the
        // `sha1` crate was not declared for this crate and D12 could not edit
        // `Cargo.toml`; that was fail-closed and correct, and it also meant the
        // Rust core could not install a package the TypeScript core could.
        //
        // SHA-1 is verified here because npm's own metadata is the only
        // integrity npm offers for these, and checking it is strictly better
        // than not: this is not a claim that SHA-1 is collision-resistant.
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

/// The `sha512-` entry of an SRI string, if there is one.
///
/// An SRI string may carry several hashes separated by whitespace, and only the
/// sha512 one is strong enough to be worth checking. A parser that took the
/// first entry would accept a `sha1-` prefix as if it were the strong hash.
fn sri_sha512(integrity: Option<&str>) -> Option<String> {
    integrity?
        .split_whitespace()
        .find_map(|entry| entry.strip_prefix("sha512-"))
        .map(str::to_string)
}

/// What a tar entry may become on disk.
///
/// A malicious tarball could otherwise ship a symlink, a hardlink or a device
/// node and use it to escape the staging directory on a LATER write, which is
/// the part that makes an allowlist rather than a denylist the right shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
}

/// ustar typeflag classification. `None` for anything that is not a plain file
/// or a directory.
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

/// Gunzip and untar ALREADY-VERIFIED bytes into `target_dir`.
///
/// Callers must have run [`verify_tarball`] over the same buffer: this function
/// trusts its input, and it is the only place the two could drift apart, which
/// is why every caller in this module does both in one breath.
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

        // The type allowlist is explicit even though `unpack_in` handles
        // containment, because `unpack_in` will happily CREATE a symlink: the
        // containment guard and the type guard defend different attacks.
        let Some(kind) = extractable_entry_type(entry.header().entry_type().as_byte()) else {
            return Err(extract_failed(format!(
                "tarball entry \"{raw}\" has unsupported type {} \
                 (symlinks/hardlinks/devices are not allowed)",
                entry.header().entry_type().as_byte(),
            )));
        };

        let out = target_dir.join(stripped);
        // Zip-slip: without this, `package/../../../bin/loca-core` overwrites
        // whatever the core can write, which includes its own binary, the
        // worker scripts and the secrets vault.
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

/// Move a staged folder into its final place, keeping the previous version
/// recoverable until the move has succeeded.
///
/// `<id>` to `<id>.old`, then `<id>.new` to `<id>`, with the old one restored
/// if the second rename fails. The failure this guards is a half-installed
/// extension: without the rollback, a failed swap leaves NO install directory
/// at all, and the extension's tools vanish with no way back.
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
                // The previous version is now stranded and there is no install
                // directory. The outer error carries only the primary failure,
                // so this is the only record of where the files went.
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

/// `path` with `suffix` appended to the whole name, not to its extension.
///
/// `Path::with_extension` would turn `@scope__name` into `@scope.old`, because
/// it treats everything after the last dot as an extension. Extension ids
/// contain dots often enough that this is a real bug rather than a hypothetical
/// one.
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
        // A file rather than a directory: the staging path is always a
        // directory, but a user can put anything under the extensions dir.
        Err(_) => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        },
    }
}

/// Copy a source tree, skipping `node_modules` at every depth.
///
/// Used by a `local` install, where the developer's folder usually has an
/// installed `node_modules` that the install phase would rebuild anyway. It is
/// also excluded from the content hash, so copying it would be bytes on disk
/// nothing ever looks at.
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
        // A symlink is neither copied nor followed. The content hash refuses
        // one outright (see `super::super::hash`), so copying it in would
        // produce an extension that installs and then cannot be verified.
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

    /// A tar built by writing the 512-byte headers directly.
    ///
    /// The `tar` crate's builder REFUSES to write a path containing `..`, which
    /// is a good default and makes it useless for staging the attack: a hostile
    /// tarball is not built by this crate. Writing the header by hand is what
    /// lets the zip-slip guard be tested against the thing it guards against
    /// rather than against a path the builder was willing to produce.
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

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, bytes).unwrap();
        gz.finish().unwrap()
    }

    // --- the integrity gate ---------------------------------------------------

    #[test]
    fn a_tarball_with_no_integrity_metadata_at_all_is_refused() {
        // The single most important row in this file. A check that degrades to
        // "no verification" when the untrusted registry metadata omits both
        // fields lets a poisoned mirror serve arbitrary bytes, and nothing
        // about the install would look wrong.
        let err = verify_tarball(b"anything", "u", &Integrity::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ChecksumMismatch);
        assert!(err.message.contains("refusing to install unverified"));
    }

    #[test]
    fn the_sha256_pin_is_checked_and_a_wrong_one_is_not_rescued_by_a_weaker_hash() {
        let bytes = b"the tarball bytes";
        let sha256 = hex::encode(sha2::Sha256::digest(bytes));

        verify_tarball(
            bytes,
            "u",
            &Integrity {
                sha256: Some(sha256.to_uppercase()),
                ..Integrity::default()
            },
        )
        .expect("an uppercase hex pin must still match");

        // A wrong sha256 alongside npm's own metadata must still fail: the
        // arms return, so the strongest present hash is the verdict. Falling
        // through would let untrusted registry metadata override a hash that
        // came from inside a signature.
        let err = verify_tarball(
            bytes,
            "u",
            &Integrity {
                sha256: Some("b".repeat(64)),
                integrity: Some(format!(
                    "sha512-{}",
                    BASE64_STANDARD.encode(sha2::Sha512::digest(bytes))
                )),
                ..Integrity::default()
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

        verify_tarball(
            bytes,
            "u",
            &Integrity {
                // A multi-entry SRI string with the strong hash second. Taking
                // the first entry would treat `sha1-...` as the strong hash and
                // compare a sha512 digest against it, which fails; taking any
                // entry that parses would accept the weak one as sufficient.
                integrity: Some(format!("sha1-{sha1} sha512-{sha512}")),
                ..Integrity::default()
            },
        )
        .expect("the sha512 entry was not found in a multi-entry SRI string");

        let err = verify_tarball(
            bytes,
            "u",
            &Integrity {
                integrity: Some(format!("sha512-{}", BASE64_STANDARD.encode([0u8; 64]))),
                ..Integrity::default()
            },
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ChecksumMismatch);

        // An SRI string carrying only weak hashes is not verification: it falls
        // through to the shasum arm, and with no shasum it is a refusal.
        let err = verify_tarball(
            bytes,
            "u",
            &Integrity {
                integrity: Some(format!("sha1-{sha1}")),
                ..Integrity::default()
            },
        )
        .unwrap_err();
        assert!(err.message.contains("refusing to install unverified"));
    }

    #[test]
    fn a_legacy_sha1_shasum_is_verified_in_both_directions() {
        // The legacy arm: an npm package old enough to publish `dist.shasum`
        // (hex SHA-1) with no `dist.integrity`. This USED to refuse outright,
        // because the `sha1` crate was not declared for this crate; that was
        // fail-closed and correct, and it also meant the port could not install
        // a package the TypeScript core could.
        //
        // Both directions, because a checksum test that only asserts the happy
        // path passes just as well against a function that ignores its input.
        let bytes = b"legacy bytes";
        let good = {
            use sha1::Digest as _;
            hex::encode(sha1::Sha1::digest(bytes))
        };
        verify_tarball(
            bytes,
            "u",
            &Integrity {
                shasum: Some(good.clone()),
                ..Integrity::default()
            },
        )
        .expect("a matching sha1 installs");

        // Upper case, since npm metadata is not case-normalised and a
        // case-sensitive compare would reject a legitimate package.
        verify_tarball(
            bytes,
            "u",
            &Integrity {
                shasum: Some(good.to_uppercase()),
                ..Integrity::default()
            },
        )
        .expect("sha1 comparison is case-insensitive");

        let err = verify_tarball(
            bytes,
            "u",
            &Integrity {
                shasum: Some("0".repeat(40)),
                ..Integrity::default()
            },
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ChecksumMismatch);
        assert!(err.message.contains("sha1"), "{}", err.message);
    }

    // --- extraction -----------------------------------------------------------

    #[test]
    fn only_plain_files_and_directories_are_extractable() {
        assert_eq!(extractable_entry_type(b'0'), Some(EntryKind::File));
        assert_eq!(extractable_entry_type(0), Some(EntryKind::File));
        assert_eq!(extractable_entry_type(b'5'), Some(EntryKind::Dir));
        // Symlink, hardlink, char device, block device, fifo, and GNU long
        // name. Each is a way to write somewhere the archive does not name.
        for flag in [b'1', b'2', b'3', b'4', b'6', b'L', b'K'] {
            assert_eq!(extractable_entry_type(flag), None, "typeflag {flag}");
        }
    }

    #[test]
    fn the_package_prefix_is_stripped_and_a_bare_prefix_entry_is_skipped() {
        assert_eq!(strip_package_prefix("package/main.ts"), Some("main.ts"));
        assert_eq!(strip_package_prefix("package/"), None);
        // Not every tarball is an npm one; an unprefixed entry passes through.
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
        // A different attack from `..`, taking a different route through the
        // same guard: `Path::join` REPLACES the base when its argument is
        // absolute, so `out` becomes the absolute path itself rather than
        // anything under the staging directory. A guard written as "does the
        // joined path start with the target" catches both; one written as
        // "does the ENTRY contain .." catches only the other one.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("staging");
        let outside = dir.path().join("absolute.txt");
        std::fs::write(&outside, b"original").unwrap();

        // The name is absolute and carries NO `package/` prefix. With the
        // prefix it would strip to a relative path and land harmlessly inside
        // the staging directory, which is what the first version of this test
        // built and why it proved nothing.
        let bytes = hostile_tar(&outside.to_string_lossy(), b"overwritten");
        let err = extract_tarball(&bytes, &target).unwrap_err();
        assert_eq!(err.code, ErrorCode::ExtractFailed);
        assert_eq!(std::fs::read(&outside).unwrap(), b"original");
    }

    #[test]
    fn a_symlink_entry_is_refused_rather_than_ignored() {
        // Ignoring it would install an extension whose archive said it wanted a
        // symlink, quietly without one. Refusing says so.
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
        // Without the rollback a failed swap leaves NO install directory: the
        // extension's tools vanish and the files are stranded under a name
        // nothing looks for.
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
        // Without this the rename to `.old` fails on every subsequent install
        // of an extension whose previous swap was interrupted.
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
        // `Path::with_extension` treats everything after the last dot as an
        // extension, so a scoped npm id would stage at `@scope.new` and the
        // swap would move the wrong directory.
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
        // And a plain file, which the staging path can be if a user put one
        // there by hand.
        let file = dir.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        rm_rf(&file).unwrap();
        assert!(!file.exists());
    }
}
