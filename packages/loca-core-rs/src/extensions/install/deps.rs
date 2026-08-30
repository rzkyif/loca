//! Install phase: `deno install` for a downloaded extension's declared
//! dependencies, then pin the content hash.
//!
//! `--allow-scripts` is gated by `extensions.ignorePostinstallScripts` (default
//! on): without it Deno denies npm lifecycle scripts, and adding it
//! unconditionally would make more packages install with nothing looking wrong.
//! Deps land in place (the folder is already swapped and no worker can spawn
//! against a `downloaded` extension); `node_modules` and `deno.lock` are
//! excluded from the hash, so pinning after install matches pristine content.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{AppError, ErrorCode, Result};

/// The `deno install` invocation for one extension folder, as data so the
/// whole decision surface is testable without a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepsInstallPlan {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// The complete environment, not inherited: Core's env can hold API keys.
    pub env: Vec<(OsString, OsString)>,
}

/// `--node-modules-dir=auto` lands `node_modules` in the folder without editing
/// the shipped `deno.json`; `DENO_DIR` keeps the cache under `~/.loca`.
pub fn deps_install_plan(
    deno: &Path,
    folder: &Path,
    deno_cache_dir: &Path,
    allow_scripts: bool,
) -> DepsInstallPlan {
    let mut args = vec![
        OsString::from("install"),
        OsString::from("--node-modules-dir=auto"),
        OsString::from("--quiet"),
    ];
    if allow_scripts {
        args.push(OsString::from("--allow-scripts"));
    }
    DepsInstallPlan {
        program: deno.to_path_buf(),
        args,
        cwd: folder.to_path_buf(),
        env: vec![(
            OsString::from("DENO_DIR"),
            deno_cache_dir.as_os_str().to_os_string(),
        )],
    }
}

/// Whether npm lifecycle scripts may run during `deno install`. The setting is
/// phrased as BLOCKING, so this is its inverse; an unresolvable record lands on
/// blocked, never allowed.
pub fn allow_postinstall_scripts(settings: &crate::settings::SettingsRecord) -> bool {
    !crate::settings::access::bool_setting(settings, "extensions.ignorePostinstallScripts", true)
}

/// Does this extension declare dependencies at all? An unparseable config is an
/// error, not a "no": skipping it would leave the extension looking installed.
pub async fn has_declared_deps(folder: &Path) -> Result<bool> {
    if let Some(text) = read_optional(&folder.join("deno.json")).await {
        let config: Value = serde_json::from_str(&text).map_err(|_| {
            AppError::new(
                ErrorCode::DepsInstallFailed,
                format!("unparseable deno.json in {}", folder.display()),
            )
        })?;
        if let Some(imports) = config.get("imports").and_then(Value::as_object) {
            // Only `npm:`/`jsr:` need installing; relative and `https:` do not.
            if imports.values().any(|spec| {
                spec.as_str()
                    .is_some_and(|spec| spec.starts_with("npm:") || spec.starts_with("jsr:"))
            }) {
                return Ok(true);
            }
        }
    }
    if let Some(text) = read_optional(&folder.join("package.json")).await {
        let package: Value = serde_json::from_str(&text).map_err(|_| {
            AppError::new(
                ErrorCode::DepsInstallFailed,
                format!("unparseable package.json in {}", folder.display()),
            )
        })?;
        if let Some(deps) = package.get("dependencies").and_then(Value::as_object) {
            if !deps.is_empty() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub async fn read_optional(path: &Path) -> Option<String> {
    tokio::fs::read_to_string(path).await.ok()
}

/// The last twelve non-blank stderr lines of a failed install, for the user's error.
pub fn stderr_tail(lines: &[String]) -> String {
    let kept: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| !line.trim().is_empty())
        .collect();
    kept[kept.len().saturating_sub(12)..].join("\n")
}

pub fn deps_failed(extension_id: &str, code: i32, tail: &str) -> AppError {
    AppError::new(
        ErrorCode::DepsInstallFailed,
        if tail.is_empty() {
            format!("deno install exited {code} for {extension_id}")
        } else {
            format!("deno install exited {code} for {extension_id}:\n{tail}")
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn plan() -> DepsInstallPlan {
        plan_with(false)
    }

    fn plan_with(allow_scripts: bool) -> DepsInstallPlan {
        deps_install_plan(
            Path::new("/home/u/.loca/stable/core/bin/deno"),
            Path::new("/home/u/.loca/stable/core/extensions/ext"),
            Path::new("/home/u/.loca/stable/core/deno-cache"),
            allow_scripts,
        )
    }

    fn args_of(plan: &DepsInstallPlan) -> Vec<String> {
        plan.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn record(v: serde_json::Value) -> crate::settings::SettingsRecord {
        match v {
            serde_json::Value::Object(map) => map,
            _ => unreachable!("a settings record is an object"),
        }
    }

    #[test]
    fn npm_lifecycle_scripts_are_off_unless_the_flag_is_passed() {
        assert_eq!(
            args_of(&plan()),
            vec!["install", "--node-modules-dir=auto", "--quiet"],
            "the argument vector changed; every addition needs its own reason"
        );
        assert_eq!(
            args_of(&plan_with(true)),
            vec![
                "install",
                "--node-modules-dir=auto",
                "--quiet",
                "--allow-scripts"
            ],
            "the opt-in must actually reach deno, or the setting is decoration"
        );
    }

    #[test]
    fn an_unset_or_hostile_record_lands_on_blocked() {
        assert!(!allow_postinstall_scripts(&record(serde_json::json!({}))));
        assert!(!allow_postinstall_scripts(&record(
            serde_json::json!({ "extensions.ignorePostinstallScripts": true })
        )));
        assert!(!allow_postinstall_scripts(&record(
            serde_json::json!({ "extensions.ignorePostinstallScripts": "yes please" })
        )));
        assert!(allow_postinstall_scripts(&record(
            serde_json::json!({ "extensions.ignorePostinstallScripts": false })
        )));
    }

    #[test]
    fn the_package_cache_stays_under_the_core_home() {
        let plan = plan();
        assert_eq!(
            plan.env,
            vec![(
                OsString::from("DENO_DIR"),
                OsString::from("/home/u/.loca/stable/core/deno-cache"),
            )]
        );
        assert_eq!(
            plan.cwd,
            Path::new("/home/u/.loca/stable/core/extensions/ext"),
            "deno install must run IN the extension folder"
        );
    }

    #[test]
    fn the_environment_is_exactly_one_key_and_not_the_cores() {
        let keys: Vec<String> = plan()
            .env
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(keys, vec!["DENO_DIR".to_string()]);
    }

    #[tokio::test]
    async fn an_extension_with_no_dependencies_needs_no_runtime() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_declared_deps(dir.path()).await.unwrap());

        tokio::fs::write(dir.path().join("deno.json"), r#"{"exports":"./main.ts"}"#)
            .await
            .unwrap();
        assert!(!has_declared_deps(dir.path()).await.unwrap());
        tokio::fs::write(
            dir.path().join("deno.json"),
            r#"{"imports":{"./x":"./x.ts","h":"https://example.com/m.ts"}}"#,
        )
        .await
        .unwrap();
        assert!(!has_declared_deps(dir.path()).await.unwrap());

        tokio::fs::write(dir.path().join("package.json"), r#"{"dependencies":{}}"#)
            .await
            .unwrap();
        assert!(!has_declared_deps(dir.path()).await.unwrap());
    }

    #[tokio::test]
    async fn npm_and_jsr_specifiers_and_package_json_dependencies_all_count() {
        for (file, body) in [
            ("deno.json", r#"{"imports":{"z":"npm:zod@3"}}"#),
            ("deno.json", r#"{"imports":{"s":"jsr:@std/path"}}"#),
            ("package.json", r#"{"dependencies":{"zod":"^3"}}"#),
        ] {
            let dir = tempfile::tempdir().unwrap();
            tokio::fs::write(dir.path().join(file), body).await.unwrap();
            assert!(
                has_declared_deps(dir.path()).await.unwrap(),
                "{file} {body} was not read as declaring dependencies"
            );
        }
    }

    #[tokio::test]
    async fn an_unparseable_config_fails_rather_than_reading_as_no_dependencies() {
        for file in ["deno.json", "package.json"] {
            let dir = tempfile::tempdir().unwrap();
            tokio::fs::write(dir.path().join(file), "{ not json")
                .await
                .unwrap();
            let err = has_declared_deps(dir.path()).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::DepsInstallFailed, "{file}");
            assert!(err.message.contains(file), "{}", err.message);
        }
    }

    #[test]
    fn a_failed_install_reports_the_end_of_its_output_and_not_all_of_it() {
        let many: Vec<String> = (0..40).map(|n| format!("line {n}")).collect();
        let tail = stderr_tail(&many);
        assert_eq!(tail.lines().count(), 12);
        assert!(tail.starts_with("line 28"), "{tail}");
        assert!(tail.ends_with("line 39"), "{tail}");

        // Blank lines are dropped before the bound.
        let padded: Vec<String> = std::iter::repeat_n(String::new(), 30)
            .chain(["the real reason".to_string()])
            .collect();
        assert_eq!(stderr_tail(&padded), "the real reason");

        assert_eq!(stderr_tail(&[]), "");
        assert!(deps_failed("ext", 1, "").message.ends_with("for ext"));
        assert!(deps_failed("ext", 1, "boom").message.ends_with(":\nboom"));
    }
}
