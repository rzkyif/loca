//! Filesystem helpers, chiefly the atomic write used for every state file.
//!
//! Work package **F4** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! Every state file is published by writing a temp file and renaming it over the
//! target, so a reader never observes a half-written file and a crash mid-write
//! leaves the previous version intact. Do not open a state file with
//! `File::create`.
//!
//! There are three writers, varying along two independent axes: what the temp
//! file is named, and what mode it is created with. The temp name is about
//! concurrent writers, the creation mode is about who can read the file, and
//! coupling them would force a caller to accept one to get the other.
//!
//! | | temp name | creation mode |
//! | --- | --- | --- |
//! | [`write_atomic`] | unique | umask |
//! | [`write_owner_atomic`] | unique | `0600` |
//! | [`write_secret_atomic`] | fixed | `0600` |
//!
//! **[`write_atomic`] is the default.** The other two exist for named reasons
//! spelled out in their own doc comments, and neither is an upgrade to reach for
//! reflexively: making ordinary state owner-only is how a settings file becomes
//! unreadable to a support script for no stated reason.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use uuid::Uuid;

use crate::error::{AppError, ErrorCode, Result};

// --- atomic writes -----------------------------------------------------------

/// Write `bytes` to `path` atomically. The default writer.
///
/// The temp file gets a unique suffix, so two concurrent writers to the same
/// path never share one. With a fixed name they interleave: one writer's rename
/// consumes the shared temp and the other's fails with `ENOENT`, a spurious
/// error unrelated to what either writer was doing. A distinct temp per writer
/// means the last rename wins cleanly. That is still a lost update at the
/// application layer, which is why the session store holds a per-session lock
/// and the settings store serializes, but it is never a spurious failure.
///
/// The file is created under the process umask. For state a non-owner has no
/// business reading, use [`write_owner_atomic`].
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
/// The unique suffix and its concurrency protection are exactly
/// [`write_atomic`]'s; the difference is `0600` at creation.
///
/// The published file is never world-readable, because the mode is applied
/// before the rename rather than after it. Chmod-ing a file the caller has
/// already renamed into place is the shape that leaves a window, and it is the
/// shape a caller falls into when the writer does not offer this mode. Setting
/// the mode at creation rather than tightening the temp afterwards narrows it
/// further: the temp file itself never exists with umask permissions either.
///
/// For the core's own credential files (`.admin-token`, `.admin-password`),
/// which must not be world-readable but do not qualify for the fixed temp name
/// [`write_secret_atomic`] uses.
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
/// target. Core passes every sandboxed child (tool workers, and MCP servers run
/// on the bundled deno) a deny list covering the vault file and its `.tmp`
/// sibling, and a deny takes precedence over any grant, so it backstops even a
/// broad one. A random suffix cannot be enumerated, so it cannot be denied, and
/// a broadly-granted tool worker could read the vault's re-encrypt temp for as
/// long as it exists.
///
/// Giving up the unique suffix gives up the concurrency protection described on
/// [`write_atomic`], so a caller qualifies only when both hold:
///
/// 1. Its path **and** that path plus `.tmp` are on the sandbox deny list.
///    Otherwise the fixed name buys nothing and the race is paid for free.
/// 2. It serializes its own writes, so the race never arises. The secrets vault
///    does: its mutations are read-modify-write cycles over one blob and are
///    chained through a lock, because concurrent writers would lose updates
///    regardless of how the temp file is named.
///
/// Today the vault (`secrets.enc`) is the only caller that qualifies.
/// `.admin-token` and `.admin-password` are on the deny list but their `.tmp`
/// siblings are not, so they take [`write_owner_atomic`]: owner-only without the
/// fixed name, which is the whole reason that third writer exists.
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
            // Best-effort cleanup so a failed write never leaks its temp. It
            // matters most on the unique-suffix branch, where nothing else would
            // ever reclaim the name.
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
            // Mode at creation, so the file is never briefly world-readable in
            // the window a chmod-after would leave open.
            opts.mode(0o600);
        }
    }

    let mut file = opts.open(tmp)?;
    file.write_all(bytes)?;
    // A rename is only atomic with respect to contents that have actually
    // reached the disk, so publish nothing that has not been synced. The Deno
    // core skipped this; the write cadence is per message rather than per
    // streamed delta, so the cost is small and a torn state file is not.
    file.sync_all()?;
    drop(file);

    if owner_only {
        tighten_mode(tmp);
    }

    // Replaces an existing destination atomically on POSIX and on Windows
    // (`MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`). On Windows it fails while
    // another process holds the destination open without `FILE_SHARE_DELETE`.
    fs::rename(tmp, path)?;
    sync_parent_dir(path);
    Ok(())
}

/// Flush the directory entry the rename just created.
///
/// `sync_all` on the temp file makes its CONTENTS durable; it says nothing
/// about the rename, which is a directory modification. Without this, a power
/// loss between the two can leave the directory entry unwritten, and the
/// atomic-write guarantee degrades from "old or new" to "old, new, or neither":
/// the file the caller just wrote is simply absent. For a settings file or a
/// session that is data loss, not a torn read.
///
/// Best-effort, and deliberately so. It is a durability improvement on a write
/// that has already fully succeeded, so a platform that refuses the directory
/// open (Windows has no equivalent, and some filesystems reject it) must not
/// turn a completed write into a reported failure.
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

/// Best-effort owner-only permissions. A chmod failure must not fail the write.
///
/// Not redundant with the mode at creation: `OpenOptions::mode` applies only
/// when the file is created, and the fixed-`.tmp` branch can reopen a temp left
/// behind by a crash, which keeps whatever mode it already had. On the
/// unique-suffix branch the temp is always newly created, so this is a no-op
/// there and kept only so both owner-only writers share one path.
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
/// A parse failure maps to `internal_error`, not `validation_error`. The blanket
/// `From<serde_json::Error>` is right for an inbound request body, where bad
/// JSON is the client's fault, and wrong for a file we wrote ourselves, where it
/// is ours. Going through this function means `?` does the right thing without
/// every call site having to remember.
///
/// A missing file surfaces as `not_found`, so a caller wanting a default matches
/// on `err.code`.
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
/// Pretty because that is what the state files this replaces carry on disk, and
/// they are meant to be hand-readable. A caller wanting compact bytes, such as a
/// derived index file, serializes itself and calls [`write_atomic`].
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

/// Run one filesystem sequence on the blocking pool. One `spawn_blocking` for
/// the whole open, write, sync, rename rather than an await per syscall:
/// `tokio::fs` is itself a `spawn_blocking` per call, and doing it once is both
/// cheaper and the only way to keep the failure-cleanup path in one place.
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

    /// Both owner-only writers, along the creation-mode axis. Verified to fail
    /// when either writer is wired to the umask instead.
    ///
    /// What it does not prove, per D-015: it asserts the published mode, which
    /// both the creation mode and the pre-rename chmod satisfy, so it does not
    /// distinguish the two. It also cannot bite under a `0077` umask, where the
    /// umask alone would produce `0600`. Neither weakens it against the
    /// regression that matters, which is the owner-only axis being dropped.
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

    /// The axes are independent, which is the point of the three-way split: the
    /// owner-only writer must not have inherited the fixed temp name along with
    /// the mode. Blocking `<path>.tmp` leaves it working.
    #[test]
    fn the_owner_only_writer_keeps_the_unique_temp_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".admin-token");
        fs::create_dir(dir.path().join(".admin-token.tmp")).unwrap();

        write_owner_atomic_blocking(&path, b"token").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"token");
        // The blocking directory is the only `.tmp` left: the writer's own temp
        // was consumed by the rename.
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

    /// The naming above, observed rather than unit-tested: blocking the fixed
    /// name stops only the secret writer.
    #[test]
    fn only_the_secret_writer_uses_the_fixed_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        // A directory cannot be opened for writing, so whichever writer wants
        // this exact name fails.
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

    /// What the unique suffix exists for. Every writer must succeed, the file
    /// must end up as exactly one writer's payload rather than a mix, and no
    /// temp may survive.
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

    /// The reason these helpers exist. Bad JSON in a file we wrote is our bug,
    /// so it must not surface as the client's `validation_error`.
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
