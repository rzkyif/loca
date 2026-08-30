//! Filesystem helpers, chiefly the atomic write used for every state file.
//!
//! Every state file is published by writing a temp file and renaming it over the
//! target, so a reader never observes a half-written file and a crash mid-write
//! leaves the previous version intact. Do not open a state file with
//! `File::create`.
//!
//! Three writers, varying along two independent axes: the temp file's name (for
//! concurrent writers) and its creation mode (for who can read it). Coupling
//! them would force a caller to accept one to get the other.
//!
//! | | temp name | creation mode |
//! | --- | --- | --- |
//! | [`write_atomic`] | unique | umask |
//! | [`write_owner_atomic`] | unique | `0600` |
//! | [`write_secret_atomic`] | fixed | `0600` |
//!
//! **[`write_atomic`] is the default.** Neither of the others is an upgrade to
//! reach for reflexively: making ordinary state owner-only is how a settings
//! file becomes unreadable to a support script for no stated reason.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::Digest;
use uuid::Uuid;

use crate::error::{AppError, ErrorCode, Result};

/// Lowercase-hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

// --- atomic writes -----------------------------------------------------------

/// Write `bytes` to `path` atomically. The default writer.
///
/// The temp file gets a unique suffix, so two concurrent writers never share
/// one: with a fixed name, one writer's rename consumes the shared temp and the
/// other fails with a spurious `ENOENT`. The last rename still wins, which is
/// why the session store holds a per-session lock and settings serializes.
///
/// Created under the process umask; for state a non-owner has no business
/// reading, use [`write_owner_atomic`].
pub async fn write_atomic(path: PathBuf, bytes: Vec<u8>) -> Result<()> {
    spawn(move || write_atomic_blocking(&path, &bytes)).await
}

/// [`write_atomic`] on the calling thread. For boot, shutdown, tests, and
/// callers with no ambient tokio runtime.
pub fn write_atomic_blocking(path: &Path, bytes: &[u8]) -> Result<()> {
    write_impl(path, bytes, &unique_tmp(path), false)
}

/// Write `bytes` to `path` atomically and owner-only, through a uniquely named
/// temp file.
///
/// [`write_atomic`]'s unique suffix and concurrency protection, with `0600` at
/// creation. The mode is applied before the rename, so the published file is
/// never briefly world-readable, and at creation rather than by a later chmod,
/// so the temp is not either.
///
/// For the core's own credential files, which must not be world-readable but do
/// not qualify for [`write_secret_atomic`]'s fixed temp name.
pub async fn write_owner_atomic(path: PathBuf, bytes: Vec<u8>) -> Result<()> {
    spawn(move || write_owner_atomic_blocking(&path, &bytes)).await
}

/// [`write_owner_atomic`] on the calling thread.
pub fn write_owner_atomic_blocking(path: &Path, bytes: &[u8]) -> Result<()> {
    write_impl(path, bytes, &unique_tmp(path), true)
}

/// Write `bytes` to `path` atomically and owner-only, through a temp file at a
/// fixed `<path>.tmp`.
///
/// The fixed name exists so the sandbox deny list can **name** the transient
/// target: a random suffix cannot be enumerated, so it cannot be denied, and a
/// broadly-granted tool worker could read the vault's re-encrypt temp.
///
/// It gives up [`write_atomic`]'s concurrency protection, so a caller qualifies
/// only when both hold:
///
/// 1. Its path **and** that path plus `.tmp` are on the sandbox deny list.
/// 2. It serializes its own writes, so the race never arises.
///
/// The vault (`secrets.enc`) is the only caller that qualifies today.
/// `.admin-token` and `.admin-password` are denied but their `.tmp` siblings are
/// not, which is why the owner-only writer exists.
pub async fn write_secret_atomic(path: PathBuf, bytes: Vec<u8>) -> Result<()> {
    spawn(move || write_secret_atomic_blocking(&path, &bytes)).await
}

/// [`write_secret_atomic`] on the calling thread. Read that function's
/// conditions before reaching for either.
pub fn write_secret_atomic_blocking(path: &Path, bytes: &[u8]) -> Result<()> {
    write_impl(path, bytes, &secret_tmp(path), true)
}

/// The one write path, parameterized by the two axes the public writers vary:
/// `tmp` is the temp-name choice, `owner_only` is the creation-mode choice.
fn write_impl(path: &Path, bytes: &[u8], tmp: &Path, owner_only: bool) -> Result<()> {
    match write_then_rename(path, bytes, tmp, owner_only) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Best-effort, so a failed write leaks no temp. It matters most on
            // the unique-suffix branch, where nothing reclaims the name.
            let _ = fs::remove_file(tmp);
            Err(err)
        }
    }
}

fn write_then_rename(path: &Path, bytes: &[u8], tmp: &Path, owner_only: bool) -> Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if owner_only {
            // Mode at creation: a chmod after would leave a window.
            opts.mode(0o600);
        }
    }

    let mut file = opts.open(tmp)?;
    file.write_all(bytes)?;
    // A rename is only atomic over contents that reached the disk, so publish
    // nothing unsynced. The write cadence is per message, so the cost is small
    // and a torn state file is not.
    file.sync_all()?;
    drop(file);

    if owner_only {
        tighten_mode(tmp);
    }

    // Atomic replacement on POSIX and on Windows (`MoveFileEx` with
    // `MOVEFILE_REPLACE_EXISTING`), though Windows fails while another process
    // holds the destination open without `FILE_SHARE_DELETE`.
    fs::rename(tmp, path)?;
    sync_parent_dir(path);
    Ok(())
}

/// Flush the directory entry the rename just created.
///
/// `sync_all` on the temp file makes its CONTENTS durable and says nothing about
/// the rename, which is a directory modification. Without this a power loss
/// between the two degrades the guarantee from "old or new" to "old, new, or
/// neither", which is data loss rather than a torn read.
///
/// Best-effort: it improves a write that already succeeded, so a platform that
/// refuses the directory open must not turn that into a failure.
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(handle) = fs::File::open(dir) {
            let _ = handle.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Best-effort owner-only permissions. Not redundant with the mode at creation:
/// `OpenOptions::mode` applies only on creation, and the fixed-`.tmp` branch can
/// reopen a temp a crash left behind, which keeps the mode it had.
fn tighten_mode(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// `<path>.<uuid>.tmp`
fn unique_tmp(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{}.tmp", Uuid::new_v4()));
    PathBuf::from(name)
}

/// `<path>.tmp`
fn secret_tmp(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

// --- JSON state files --------------------------------------------------------

/// Read and parse a JSON state file.
///
/// A parse failure maps to `internal_error`, not the blanket
/// `From<serde_json::Error>`'s `validation_error`: bad JSON in a file we wrote
/// is our bug, not the client's. A missing file is `not_found`, so a caller
/// wanting a default matches on `err.code`.
pub async fn read_json<T: DeserializeOwned + Send + 'static>(path: PathBuf) -> Result<T> {
    spawn(move || read_json_blocking(&path)).await
}

/// [`read_json`] on the calling thread.
pub fn read_json_blocking<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|err| {
        AppError::new(
            ErrorCode::InternalError,
            format!("failed to parse {}: {err}", path.display()),
        )
    })
}

/// Serialize `value` as pretty-printed JSON and write it through
/// [`write_atomic`].
///
/// Pretty, because these files are meant to be hand-readable. A caller wanting
/// compact bytes serializes itself and calls [`write_atomic`].
pub async fn write_json_atomic<T: Serialize>(path: PathBuf, value: &T) -> Result<()> {
    write_atomic(path, to_pretty_bytes(value)?).await
}

/// [`write_json_atomic`] on the calling thread.
pub fn write_json_atomic_blocking<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_atomic_blocking(path, &to_pretty_bytes(value)?)
}

fn to_pretty_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(value).map_err(|err| {
        AppError::new(
            ErrorCode::InternalError,
            format!("failed to serialize state file: {err}"),
        )
    })
}

// --- shared -------------------------------------------------------------------

/// Run one filesystem sequence on the blocking pool: one `spawn_blocking` for
/// the whole open, write, sync, rename, rather than `tokio::fs`'s one per
/// syscall, which also keeps the failure cleanup in one place.
async fn spawn<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|err| {
        AppError::new(
            ErrorCode::InternalError,
            format!("filesystem task failed: {err}"),
        )
    })?
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn write_and_rewrite_round_trip_with_no_leftover_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");

        write_atomic_blocking(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");

        write_atomic_blocking(&path, b"world").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"world");

        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn bytes_round_trip_including_nul_and_high_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.bin");
        let bytes = [0u8, 1, 2, 255];
        write_atomic_blocking(&path, &bytes).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    /// Both owner-only writers, along the creation-mode axis. It asserts the
    /// published mode, which the creation mode and the pre-rename chmod both
    /// satisfy, and cannot bite under a `0077` umask; neither weakens it
    /// against the regression that matters.
    #[cfg(unix)]
    #[test]
    fn the_owner_only_writers_publish_an_owner_only_file() {
        let dir = tempfile::tempdir().unwrap();

        let secret = dir.path().join("secrets.enc");
        write_secret_atomic_blocking(&secret, &[1]).unwrap();
        assert_eq!(mode_of(&secret), 0o600);

        let admin = dir.path().join(".admin-password");
        write_owner_atomic_blocking(&admin, b"hunter2").unwrap();
        assert_eq!(mode_of(&admin), 0o600);
    }

    /// The axes are independent: the owner-only writer must not have inherited
    /// the fixed temp name along with the mode.
    #[test]
    fn the_owner_only_writer_keeps_the_unique_temp_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".admin-token");
        fs::create_dir(dir.path().join(".admin-token.tmp")).unwrap();

        write_owner_atomic_blocking(&path, b"token").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"token");
        // The blocking directory is the only `.tmp` left.
        assert_eq!(leftovers(dir.path()), vec![".admin-token.tmp".to_string()]);
    }

    #[test]
    fn the_temp_name_is_fixed_for_secrets_and_unique_otherwise() {
        let path = Path::new("/tmp/secrets.enc");
        assert_eq!(secret_tmp(path), Path::new("/tmp/secrets.enc.tmp"));

        let a = unique_tmp(path);
        let b = unique_tmp(path);
        assert_ne!(a, b, "two ordinary writers would share one temp file");
        for tmp in [&a, &b] {
            let name = tmp.to_string_lossy();
            assert!(name.starts_with("/tmp/secrets.enc."));
            assert!(name.ends_with(".tmp"));
        }
    }

    /// Blocking the fixed name stops only the secret writer.
    #[test]
    fn only_the_secret_writer_uses_the_fixed_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        // A directory cannot be opened for writing.
        fs::create_dir(dir.path().join("state.tmp")).unwrap();

        assert!(write_secret_atomic_blocking(&path, b"x").is_err());
        write_atomic_blocking(&path, b"x").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"x");
    }

    /// Reachable only on the fixed-name branch: a crash mid-write leaves the
    /// temp behind, and `OpenOptions::mode` does not apply to a reopened file.
    #[cfg(unix)]
    #[test]
    fn a_stale_secret_temp_is_still_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.enc");
        let stale = dir.path().join("secrets.enc.tmp");

        fs::write(&stale, b"leftover").unwrap();
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o644)).unwrap();

        write_secret_atomic_blocking(&path, b"fresh").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"fresh");
        assert_eq!(mode_of(&path), 0o600);
    }

    /// What the unique suffix exists for: every writer succeeds, the file is
    /// exactly one writer's payload rather than a mix, and no temp survives.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writes_to_one_path_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contended");

        let payloads: Vec<Vec<u8>> = (0..24).map(|i| vec![b'a' + i as u8; 512]).collect();
        let mut tasks = Vec::new();
        for payload in payloads.clone() {
            let path = path.clone();
            tasks.push(tokio::spawn(
                async move { write_atomic(path, payload).await },
            ));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }

        let final_bytes = fs::read(&path).unwrap();
        assert!(
            payloads.contains(&final_bytes),
            "the published file is not any single writer's payload"
        );
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn a_failed_write_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-dir/state.json");
        assert!(write_atomic_blocking(&path, b"x").is_err());
        assert!(!dir.path().join("missing-dir").exists());
        assert!(leftovers(dir.path()).is_empty());
    }

    // --- JSON state files ----------------------------------------------------

    #[test]
    fn json_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let value = serde_json::json!({ "server.bindHost": "0.0.0.0", "llm.topK": 40 });

        write_json_atomic_blocking(&path, &value).unwrap();
        let read: serde_json::Value = read_json_blocking(&path).unwrap();
        assert_eq!(read, value);
        // Pretty, because these files are meant to be hand-readable.
        assert!(fs::read_to_string(&path).unwrap().contains('\n'));
    }

    /// Bad JSON in a file we wrote is our bug, not a `validation_error`.
    #[test]
    fn a_corrupt_state_file_is_internal_not_a_validation_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{ not json").unwrap();

        let err = read_json_blocking::<serde_json::Value>(&path).unwrap_err();
        assert_eq!(err.code, ErrorCode::InternalError);
    }

    #[test]
    fn a_missing_state_file_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            read_json_blocking::<serde_json::Value>(&dir.path().join("absent.json")).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }
}
