//! The update marker: the one piece of state that survives the swap.
//!
//! Written by `apply` immediately before it restarts the core
//! (`self-updater.ts:239-242`), read by the NEXT boot, and deleted once that
//! boot proves the new binary works. It is the whole of the handshake between
//! the binary that staged an update and the binary that has to justify itself.
//!
//! # It crosses the cutover, so the field names are the contract
//!
//! A Deno core can stage an update that a Rust core reads, and the other way
//! round, so `version` / `previousVersion` / `attempts` are exactly the keys
//! `writeUpdateMarker` writes (`rollback.ts:51-56`). Key ORDER is not part of
//! it: both sides parse rather than compare bytes, unlike the sealed vault
//! (D-048), so this is a name-level contract and not a byte-level one.
//!
//! # An unreadable marker is not the same as no marker
//!
//! `readMarker` (`rollback.ts:58-65`) catches everything and returns null, so
//! absent, unreadable and malformed all arrive at the caller as "no update is
//! pending". D-061 is the record of what that collapse costs: a cache read that
//! meant four things sent a route to the network. Here the COLLAPSE is right
//! (continuing to boot is the safe answer for all three) and the SILENCE is
//! not, because this code runs before anything is serving and the log is the
//! only evidence anyone gets. So [`read`] distinguishes them and the caller
//! logs the difference.
//!
//! A malformed marker is deliberately left on disk rather than deleted here.
//! Deleting it would destroy the evidence before anyone reads it, and it heals
//! by itself: `commit_update` removes the marker on the first boot that reaches
//! the healthy checkpoint, which is exactly the boot after which it no longer
//! matters.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::paths::Paths;

/// `UpdateMarker` from `rollback.ts:40-49`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMarker {
    /// The version the updater intended to install, which equals the running
    /// binary's own version on a healthy first boot. That equality IS the
    /// check: it is how the next boot knows the marker is about itself.
    pub version: String,
    /// The version that was replaced, and therefore the version of the
    /// `<bin>.old` anchor.
    pub previous_version: String,
    /// Boots that have seen this marker without committing. Bumped on every
    /// boot until the new binary reaches its healthy checkpoint.
    pub attempts: u32,
}

/// What was on disk.
#[derive(Debug)]
pub enum MarkerState {
    /// No update is pending. The overwhelmingly common case, and the one that
    /// must stay silent: a line per boot would bury the two that matter.
    Absent,
    /// Present and not usable. Carries the reason, because this is the only
    /// place it will ever be said.
    Unreadable(String),
    Present(UpdateMarker),
}

/// Write a fresh marker with `attempts: 0`.
///
/// Called after the binary has been swapped and before anything restarts, so a
/// crash between the two leaves a marker for a binary that is already in place.
/// That ordering is what makes the crash recoverable rather than invisible.
pub async fn write(paths: &Paths, version: &str, previous_version: &str) -> Result<()> {
    let marker = UpdateMarker {
        version: version.to_string(),
        previous_version: previous_version.to_string(),
        attempts: 0,
    };
    crate::files::write_json_atomic(paths.update_marker_file(), &marker).await
}

/// Read the marker, keeping absent apart from unreadable.
pub async fn read(paths: &Paths) -> MarkerState {
    let path = paths.update_marker_file();
    match crate::files::read_json::<UpdateMarker>(path).await {
        Ok(marker) => MarkerState::Present(marker),
        Err(err) if err.code == crate::error::ErrorCode::NotFound => MarkerState::Absent,
        Err(err) => MarkerState::Unreadable(err.message),
    }
}

/// Record that another boot has seen this marker.
pub async fn bump(paths: &Paths, marker: &UpdateMarker) -> Result<()> {
    let bumped = UpdateMarker {
        attempts: marker.attempts + 1,
        ..marker.clone()
    };
    crate::files::write_json_atomic(paths.update_marker_file(), &bumped).await
}

/// Remove the marker. Best-effort: every caller is on a path where a failure to
/// delete must not change the decision that was already made.
pub async fn delete(paths: &Paths) {
    let path = paths.update_marker_file();
    if let Err(err) = tokio::fs::remove_file(&path).await {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                target: "loca::update.rollback",
                "could not remove the update marker at {}: {err}",
                path.display(),
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::Channel;

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path().join("core"));
        paths.ensure_dirs().unwrap();
        let _ = Channel::Stable;
        (dir, paths)
    }

    #[tokio::test]
    async fn a_written_marker_reads_back_with_the_keys_the_other_core_writes() {
        // The field names are the cutover contract: a Deno core can stage the
        // update a Rust core reads. Asserted on the raw JSON rather than on the
        // struct, because a round trip through serde agrees with itself
        // whatever the names are, which is the lens problem D-048 names.
        let (_dir, paths) = paths();
        write(&paths, "0.2.0", "0.1.15").await.unwrap();

        let raw = tokio::fs::read_to_string(paths.update_marker_file())
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["version"], "0.2.0");
        assert_eq!(value["previousVersion"], "0.1.15");
        assert_eq!(value["attempts"], 0);
        assert_eq!(
            value.as_object().unwrap().len(),
            3,
            "a fourth key would be one the other core drops on the way back"
        );
    }

    #[tokio::test]
    async fn a_marker_written_by_the_other_core_is_read() {
        // The other half of the same contract, driven from bytes rather than
        // from our own writer, so this cannot pass by agreeing with itself.
        let (_dir, paths) = paths();
        tokio::fs::write(
            paths.update_marker_file(),
            r#"{"version":"0.3.0","previousVersion":"0.2.0","attempts":1}"#,
        )
        .await
        .unwrap();

        let MarkerState::Present(marker) = read(&paths).await else {
            panic!("d13: a well-formed marker did not read back");
        };
        assert_eq!(marker.version, "0.3.0");
        assert_eq!(marker.previous_version, "0.2.0");
        assert_eq!(marker.attempts, 1);
    }

    #[tokio::test]
    async fn absent_and_unreadable_are_different_answers() {
        // D-061's shape. Both continue the boot, which is right, but only one
        // of them is worth a log line, and this code runs before anything can
        // report anything else.
        let (_dir, paths) = paths();
        assert!(matches!(read(&paths).await, MarkerState::Absent));

        tokio::fs::write(paths.update_marker_file(), "{ this is not json")
            .await
            .unwrap();
        let MarkerState::Unreadable(reason) = read(&paths).await else {
            panic!("d13: a malformed marker was reported as absent");
        };
        assert!(!reason.is_empty(), "the reason is the whole point");
    }

    #[tokio::test]
    async fn a_marker_missing_a_field_is_unreadable_rather_than_defaulted() {
        // Defaulting `attempts` to 0 would restart the retry budget on every
        // boot, so a binary that never comes up would never be rolled back:
        // the budget would reset faster than it could be spent.
        let (_dir, paths) = paths();
        tokio::fs::write(
            paths.update_marker_file(),
            r#"{"version":"0.3.0","previousVersion":"0.2.0"}"#,
        )
        .await
        .unwrap();
        assert!(matches!(read(&paths).await, MarkerState::Unreadable(_)));
    }

    #[tokio::test]
    async fn bumping_preserves_both_versions() {
        // The versions are what the next boot decides on, so a bump that lost
        // one would turn a pending update into an unrelated marker and clear it.
        let (_dir, paths) = paths();
        write(&paths, "0.2.0", "0.1.15").await.unwrap();
        let MarkerState::Present(marker) = read(&paths).await else {
            panic!("d13: no marker");
        };
        bump(&paths, &marker).await.unwrap();

        let MarkerState::Present(bumped) = read(&paths).await else {
            panic!("d13: no marker after bump");
        };
        assert_eq!(bumped.attempts, 1);
        assert_eq!(bumped.version, "0.2.0");
        assert_eq!(bumped.previous_version, "0.1.15");
    }

    #[tokio::test]
    async fn deleting_an_absent_marker_is_not_an_error() {
        let (_dir, paths) = paths();
        delete(&paths).await;
        assert!(matches!(read(&paths).await, MarkerState::Absent));
    }
}
