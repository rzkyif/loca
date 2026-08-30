//! Extension installation: download (fetch + validate `loca.json`, rows at
//! `downloaded`) and install (`deno install` deps, pin the hash, `installed`).
//! Kept apart so the built-in can be seeded offline at boot and installed later.

pub mod deps;
pub mod download;
pub mod manifest;
pub mod net;
pub mod npm;
pub mod register;
pub mod runner;
pub mod seed;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::error::{AppError, ErrorCode, Result};

/// Where an extension's bytes come from.
#[derive(Debug, Clone)]
pub enum InstallSource {
    Npm {
        name: String,
        version: Option<String>,
    },
    Local {
        path: String,
        slug: String,
    },
    /// One of the extensions Loca ships. `planted` set means first-boot
    /// seeding, fully offline from the planted tarball + verified manifest;
    /// unset resolves from the codebase (dev) or the signed CDN tarball, which
    /// fetches and so runs only on a user action.
    Seeded {
        id: String,
        planted: Option<Box<Planted>>,
    },
}

#[derive(Debug, Clone)]
pub struct Planted {
    pub tarball_path: std::path::PathBuf,
    pub manifest: manifest::SeededManifest,
}

impl InstallSource {
    /// The extension id this spec installs under.
    pub fn extension_id(&self) -> String {
        match self {
            Self::Npm { name, .. } => register::flatten_npm_name(name),
            Self::Local { slug, .. } => slug.clone(),
            Self::Seeded { id, .. } => id.clone(),
        }
    }

    /// The `source` column value.
    pub fn source_name(&self) -> &'static str {
        match self {
            Self::Npm { .. } => "npm",
            Self::Local { .. } => "local",
            Self::Seeded { .. } => "seeded",
        }
    }
}

/// Per-extension in-flight guard: concurrent jobs for one id share the `.new`
/// and `.old` staging paths and would wipe each other's extract. A handle
/// rather than a global so two cores in one test binary do not share it.
#[derive(Debug, Default)]
pub struct InFlight {
    ids: Mutex<HashSet<String>>,
}

impl InFlight {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim `extension_id`, or refuse. Releases on drop, including on panic.
    pub fn claim(&self, extension_id: &str) -> Result<InFlightGuard<'_>> {
        let mut ids = lock(&self.ids);
        if !ids.insert(extension_id.to_string()) {
            return Err(already_in_progress(extension_id));
        }
        Ok(InFlightGuard {
            owner: self,
            extension_id: extension_id.to_string(),
        })
    }

    /// Owning form of [`InFlight::claim`], for guards that cross into a spawned task.
    pub fn claim_owned(self: Arc<Self>, extension_id: &str) -> Result<OwnedInFlightGuard> {
        {
            let mut ids = lock(&self.ids);
            if !ids.insert(extension_id.to_string()) {
                return Err(already_in_progress(extension_id));
            }
        }
        Ok(OwnedInFlightGuard {
            owner: self,
            extension_id: extension_id.to_string(),
        })
    }

    pub fn is_busy(&self, extension_id: &str) -> bool {
        lock(&self.ids).contains(extension_id)
    }
}

fn already_in_progress(extension_id: &str) -> AppError {
    AppError::new(
        ErrorCode::Conflict,
        format!("an install/update for \"{extension_id}\" is already in progress"),
    )
}

/// The owning half of [`InFlight::claim_owned`].
#[derive(Debug)]
pub struct OwnedInFlightGuard {
    owner: Arc<InFlight>,
    extension_id: String,
}

impl Drop for OwnedInFlightGuard {
    fn drop(&mut self) {
        lock(&self.owner.ids).remove(&self.extension_id);
    }
}

#[derive(Debug)]
pub struct InFlightGuard<'a> {
    #[allow(dead_code)]
    owner: &'a InFlight,
    extension_id: String,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        lock(&self.owner.ids).remove(&self.extension_id);
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_second_job_for_the_same_extension_is_refused_while_the_first_holds_it() {
        let inflight = InFlight::new();
        let held = inflight.claim("ext").unwrap();
        let err = inflight.claim("ext").unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        assert!(inflight.is_busy("ext"));

        let other = inflight.claim("other").unwrap();
        drop(other);

        drop(held);
        assert!(!inflight.is_busy("ext"));
        inflight.claim("ext").expect("the claim was never released");
    }

    #[test]
    fn a_job_that_fails_still_releases_its_claim() {
        let inflight = InFlight::new();
        let failed = |inflight: &InFlight| -> Result<()> {
            let _guard = inflight.claim("ext")?;
            Err(AppError::new(ErrorCode::InternalError, "boom"))
        };
        assert!(failed(&inflight).is_err());
        assert!(!inflight.is_busy("ext"), "a failed job kept its claim");
    }

    #[test]
    fn a_panicking_job_releases_its_claim_too() {
        let inflight = InFlight::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = inflight.claim("ext").unwrap();
            panic!("boom");
        }));
        assert!(result.is_err());
        assert!(!inflight.is_busy("ext"));
    }

    #[test]
    fn an_install_spec_resolves_to_the_id_its_files_land_under() {
        assert_eq!(
            InstallSource::Npm {
                name: "@acme/thing".to_string(),
                version: None,
            }
            .extension_id(),
            "@acme__thing"
        );
        assert_eq!(
            InstallSource::Local {
                path: "/tmp/x".to_string(),
                slug: "my-ext".to_string(),
            }
            .extension_id(),
            "my-ext"
        );
        assert_eq!(
            InstallSource::Seeded {
                id: seed::BUILTIN_EXTENSION_ID.to_string(),
                planted: None,
            }
            .extension_id(),
            "loca-extension-builtin"
        );
    }

    #[test]
    fn the_source_column_matches_what_the_registry_stores() {
        for (spec, expected) in [
            (
                InstallSource::Npm {
                    name: "x".to_string(),
                    version: None,
                },
                "npm",
            ),
            (
                InstallSource::Local {
                    path: "/x".to_string(),
                    slug: "x".to_string(),
                },
                "local",
            ),
            (
                InstallSource::Seeded {
                    id: "x".to_string(),
                    planted: None,
                },
                "seeded",
            ),
        ] {
            assert_eq!(spec.source_name(), expected);
        }
    }
}
