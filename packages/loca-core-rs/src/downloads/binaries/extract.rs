//! Unpacking a downloaded binary archive.
//!
//! One format, `.zip`, because every `deno` release asset is a `.zip` on every
//! triple. The tar/gzip path, the SONAME symlink chain and the companion
//! libraries-only extractor are dropped with the rest of the sidecar matrix
//! (plan section 4.2); nothing a `deno`-only core can reach used them.
//!
//! # The basename rule IS the zip-slip defence
//!
//! The executable is located **by basename, wherever it sits in the archive**,
//! and it is written to `<target_dir>/<exe_name>`: a path this code builds, never
//! one the archive supplies. An entry called `../../../etc/cron.d/evil`
//! therefore has basename `evil`, does not match the executable name, and is
//! ignored; and even a matching entry cannot steer the destination, because the
//! destination is not read from the entry at all.
//!
//! That is a complete defence against a malicious archive writing outside the
//! target directory, and **nothing in the TypeScript says so**. It was arrived
//! at as a side effect of wanting a lenient archive layout (the release asset
//! nests the executable under `bin/` on some triples and at the root on others),
//! so a later reader looking at it sees only the leniency and can "simplify" it
//! into `target_dir.join(entry.name())`, which is the hole. It is preserved
//! deliberately, it is stated here, and
//! `a_zip_entry_cannot_write_outside_the_target_dir` fails if it is undone.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{AppError, ErrorCode, Result};

/// Unpack the executable named `exe_name` out of the `.zip` at `archive_path`
/// into `target_dir`.
///
/// Extra entries (READMEs, licenses, completions) are ignored rather than
/// refused: the release asset's layout is not ours and varies by triple.
pub fn extract_zip(archive_path: &Path, target_dir: &Path, exe_name: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(target_dir).map_err(|err| {
        AppError::new(
            ErrorCode::ExtractFailed,
            format!("could not create {}: {err}", target_dir.display()),
        )
    })?;
    let file = std::fs::File::open(archive_path).map_err(|err| {
        AppError::new(
            ErrorCode::ExtractFailed,
            format!("could not open {}: {err}", archive_path.display()),
        )
    })?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|err| AppError::new(ErrorCode::ExtractFailed, format!("bad zip: {err}")))?;

    let destination = target_dir.join(exe_name);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|err| {
            AppError::new(ErrorCode::ExtractFailed, format!("bad zip entry: {err}"))
        })?;
        if entry.is_dir() {
            continue;
        }
        // The basename, from the archive's own name, is used ONLY to decide
        // whether this is the entry we want. It never becomes part of a path.
        // See the module docs: this is the containment control, not a
        // convenience.
        let name = entry.name().to_string();
        let base = name.rsplit(['/', '\\']).next().unwrap_or(&name);
        if base != exe_name {
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|err| {
            AppError::new(
                ErrorCode::ExtractFailed,
                format!("could not read {name} from the archive: {err}"),
            )
        })?;
        write_executable(&destination, &bytes)?;
        return Ok(destination);
    }
    Err(AppError::new(
        ErrorCode::ExtractFailed,
        format!("archive {} is missing {exe_name}", archive_path.display()),
    ))
}

/// Write the executable and make it runnable.
///
/// The mode is set on unix only, and it is set explicitly rather than inherited
/// from the zip entry: an archive that ships mode 0644 for its own executable
/// would otherwise install a binary that cannot be spawned, and the failure
/// would surface much later as a confusing permission error at boot.
fn write_executable(destination: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(destination, bytes).map_err(|err| {
        AppError::new(
            ErrorCode::ExtractFailed,
            format!("could not write {}: {err}", destination.display()),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755)).map_err(
            |err| {
                AppError::new(
                    ErrorCode::ExtractFailed,
                    format!("could not chmod {}: {err}", destination.display()),
                )
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Build a zip whose entries are `(name, contents)`.
    ///
    /// `FileOptions` with a large-file flag off and stored-or-deflated left to
    /// the default, because what these tests exercise is entry NAMING, not
    /// compression.
    fn zip_with(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asset.zip");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, contents) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
        (dir, path)
    }

    #[test]
    fn the_executable_is_found_wherever_it_sits_and_extras_are_ignored() {
        // The leniency the basename rule exists for: the release asset nests the
        // executable on some triples and not others.
        let (_dir, archive) = zip_with(&[
            ("README.md", b"ignore me"),
            ("deno-x86_64/bin/deno", b"EXE-BYTES"),
            ("LICENSE", b"ignore me too"),
        ]);
        let target = tempfile::tempdir().unwrap();

        let written = extract_zip(&archive, target.path(), "deno").unwrap();

        assert_eq!(written, target.path().join("deno"));
        assert_eq!(std::fs::read(&written).unwrap(), b"EXE-BYTES");
        assert!(!target.path().join("README.md").exists());
    }

    #[test]
    fn a_zip_entry_cannot_write_outside_the_target_dir() {
        // The property the TypeScript never states. Two shapes: a traversing
        // path whose basename does not match (ignored outright), and one whose
        // basename DOES match, which must still land at the path this code
        // built rather than the one the archive asked for.
        let (_dir, archive) = zip_with(&[
            ("../../../etc/cron.d/evil", b"PWNED"),
            ("../../../../tmp/deno", b"EXE-BYTES"),
        ]);
        let target = tempfile::tempdir().unwrap();
        let outside = target.path().parent().unwrap().join("deno");
        let _ = std::fs::remove_file(&outside);

        let written = extract_zip(&archive, target.path(), "deno").unwrap();

        assert_eq!(
            written,
            target.path().join("deno"),
            "the archive steered the destination"
        );
        assert_eq!(std::fs::read(&written).unwrap(), b"EXE-BYTES");
        assert!(
            !outside.exists(),
            "an entry escaped the target directory: {}",
            outside.display()
        );
        assert!(
            !Path::new("/etc/cron.d/evil").exists(),
            "a traversing entry was written"
        );
    }

    #[test]
    fn an_absolute_entry_path_cannot_steer_the_destination_either() {
        let (_dir, archive) = zip_with(&[("/tmp/deno", b"EXE-BYTES")]);
        let target = tempfile::tempdir().unwrap();

        let written = extract_zip(&archive, target.path(), "deno").unwrap();

        assert_eq!(written, target.path().join("deno"));
        assert_eq!(std::fs::read(&written).unwrap(), b"EXE-BYTES");
    }

    #[test]
    fn a_backslash_separated_entry_is_split_on_the_backslash_too() {
        // A zip written on Windows can carry backslashes despite the spec, and
        // splitting only on `/` would make the basename the whole path, which
        // then never matches and the install fails with "missing deno".
        let (_dir, archive) = zip_with(&[("deno-x86_64\\bin\\deno", b"EXE-BYTES")]);
        let target = tempfile::tempdir().unwrap();

        let written = extract_zip(&archive, target.path(), "deno").unwrap();

        assert_eq!(std::fs::read(&written).unwrap(), b"EXE-BYTES");
    }

    #[test]
    fn an_archive_without_the_executable_is_refused() {
        let (_dir, archive) = zip_with(&[("README.md", b"nothing useful")]);
        let target = tempfile::tempdir().unwrap();

        let err = extract_zip(&archive, target.path(), "deno").unwrap_err();

        assert_eq!(err.code, ErrorCode::ExtractFailed);
        assert!(err.message.contains("missing deno"), "{}", err.message);
    }

    #[cfg(unix)]
    #[test]
    fn the_extracted_executable_is_runnable() {
        // An archive that ships 0644 for its own executable would otherwise
        // install a binary that cannot be spawned, and the failure would surface
        // much later as a confusing permission error at boot.
        use std::os::unix::fs::PermissionsExt;
        let (_dir, archive) = zip_with(&[("deno", b"EXE-BYTES")]);
        let target = tempfile::tempdir().unwrap();

        let written = extract_zip(&archive, target.path(), "deno").unwrap();

        let mode = std::fs::metadata(&written).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "not executable: {mode:o}");
    }

    #[test]
    fn a_file_that_is_not_a_zip_is_an_extract_failure_rather_than_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("asset.zip");
        std::fs::write(&archive, b"not a zip at all").unwrap();
        let target = tempfile::tempdir().unwrap();

        let err = extract_zip(&archive, target.path(), "deno").unwrap_err();

        assert_eq!(err.code, ErrorCode::ExtractFailed);
    }
}
