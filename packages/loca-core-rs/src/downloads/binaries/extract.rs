//! Unpacking a downloaded binary archive. One format, `.zip`, because every
//! `deno` release asset is a `.zip` on every triple.
//!
//! The basename rule IS the zip-slip defence: the executable is located by
//! basename wherever it sits, and written to `<target_dir>/<exe_name>`, a path
//! this code builds, never one the archive supplies. Do not "simplify" it into
//! `target_dir.join(entry.name())`; `a_zip_entry_cannot_write_outside_the_target_dir`
//! fails if that happens.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{AppError, ErrorCode, Result};

/// Unpack the executable named `exe_name` out of the `.zip` at `archive_path`
/// into `target_dir`. Extra entries are ignored: the asset layout varies by triple.
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
        // The basename decides only whether this is the wanted entry; it never
        // becomes part of a path.
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

/// Write the executable and make it runnable. The mode is set explicitly (unix)
/// rather than inherited, so an archive shipping 0644 still installs a spawnable binary.
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

    /// Extract `deno` from a zip with `entries` into a fresh target dir.
    fn extract(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, Result<PathBuf>) {
        let (_dir, archive) = zip_with(entries);
        let target = tempfile::tempdir().unwrap();
        let result = extract_zip(&archive, target.path(), "deno");
        (target, result)
    }

    #[test]
    fn the_executable_is_found_wherever_it_sits_and_extras_are_ignored() {
        let (target, written) = extract(&[
            ("README.md", b"ignore me"),
            ("deno-x86_64/bin/deno", b"EXE-BYTES"),
            ("LICENSE", b"ignore me too"),
        ]);
        let written = written.unwrap();

        assert_eq!(written, target.path().join("deno"));
        assert_eq!(std::fs::read(&written).unwrap(), b"EXE-BYTES");
        assert!(!target.path().join("README.md").exists());
    }

    #[test]
    fn a_zip_entry_cannot_write_outside_the_target_dir() {
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
        let (target, written) = extract(&[("/tmp/deno", b"EXE-BYTES")]);
        let written = written.unwrap();

        assert_eq!(written, target.path().join("deno"));
        assert_eq!(std::fs::read(&written).unwrap(), b"EXE-BYTES");
    }

    #[test]
    fn a_backslash_separated_entry_is_split_on_the_backslash_too() {
        let (_target, written) = extract(&[("deno-x86_64\\bin\\deno", b"EXE-BYTES")]);

        assert_eq!(std::fs::read(written.unwrap()).unwrap(), b"EXE-BYTES");
    }

    #[test]
    fn an_archive_without_the_executable_is_refused() {
        let (_target, err) = extract(&[("README.md", b"nothing useful")]);
        let err = err.unwrap_err();

        assert_eq!(err.code, ErrorCode::ExtractFailed);
        assert!(err.message.contains("missing deno"), "{}", err.message);
    }

    #[cfg(unix)]
    #[test]
    fn the_extracted_executable_is_runnable() {
        use std::os::unix::fs::PermissionsExt;
        let (_target, written) = extract(&[("deno", b"EXE-BYTES")]);

        let mode = std::fs::metadata(written.unwrap())
            .unwrap()
            .permissions()
            .mode();
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
