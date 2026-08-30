//! The update marker: written by `apply` before the restart, read by the NEXT
//! boot, deleted once that boot proves the new binary works.
//!
//! The field names (`version` / `previousVersion` / `attempts`) are the
//! contract between the binary that staged an update and the one that boots.
//! [`read`] keeps absent apart from unreadable: both continue the boot, but
//! only one deserves a log line, and a malformed marker is left on disk as
//! evidence (`commit_update` removes it on the first healthy boot).

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::paths::Paths;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMarker {
    /// The version the updater intended to install; equals the running
    /// binary's own version on a healthy first boot, which IS the check.
    pub version: String,
    /// The version that was replaced, and therefore of the `<bin>.old` anchor.
    pub previous_version: String,
    /// Boots that have seen this marker without committing.
    pub attempts: u32,
}

/// What was on disk.
#[derive(Debug)]
pub enum MarkerState {
    Absent,
    /// Present and not usable, with the reason.
    Unreadable(String),
    Present(UpdateMarker),
}

/// Write a fresh marker with `attempts: 0`.
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

/// Remove the marker. Best-effort: a failure to delete must not change the
/// decision already made.
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

    async fn present(paths: &Paths) -> UpdateMarker {
        let MarkerState::Present(marker) = read(paths).await else {
            panic!("no marker");
        };
        marker
    }

    #[tokio::test]
    async fn a_written_marker_reads_back_with_the_keys_the_other_core_writes() {
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
            "a fourth key would be one the other core drops"
        );
    }

    #[tokio::test]
    async fn a_marker_written_by_the_other_core_is_read() {
        let (_dir, paths) = paths();
        tokio::fs::write(
            paths.update_marker_file(),
            r#"{"version":"0.3.0","previousVersion":"0.2.0","attempts":1}"#,
        )
        .await
        .unwrap();

        let marker = present(&paths).await;
        assert_eq!(marker.version, "0.3.0");
        assert_eq!(marker.previous_version, "0.2.0");
        assert_eq!(marker.attempts, 1);
    }

    #[tokio::test]
    async fn absent_and_unreadable_are_different_answers() {
        let (_dir, paths) = paths();
        assert!(matches!(read(&paths).await, MarkerState::Absent));

        tokio::fs::write(paths.update_marker_file(), "{ this is not json")
            .await
            .unwrap();
        let MarkerState::Unreadable(reason) = read(&paths).await else {
            panic!("a malformed marker was reported as absent");
        };
        assert!(!reason.is_empty());
    }

    #[tokio::test]
    async fn a_marker_missing_a_field_is_unreadable_rather_than_defaulted() {
        // Defaulting `attempts` would reset the retry budget every boot.
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
        let (_dir, paths) = paths();
        write(&paths, "0.2.0", "0.1.15").await.unwrap();
        bump(&paths, &present(&paths).await).await.unwrap();

        let bumped = present(&paths).await;
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
