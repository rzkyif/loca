//! Install phase: `deno install` for a downloaded extension's declared
//! dependencies, then pin the content hash.
//!
//! Ports `packages/loca-core/src/extensions/installer-deps.ts`.
//!
//! # `--allow-scripts` is a setting, and off is the default
//!
//! Without it Deno denies npm lifecycle scripts, which is the difference
//! between installing a package and executing whatever its author put in
//! `postinstall`. That is why `extensions.ignorePostinstallScripts` defaults to
//! on and why the flag's presence is asserted by a test rather than left to
//! review: adding it unconditionally would make MORE packages install
//! successfully and nothing would look wrong.
//!
//! It is a setting rather than a constant because the panel already offered it,
//! with copy telling the user to turn it off when an extension needs to build
//! native code, and nothing read the key in either core. A switch that promises
//! an escape hatch and does not open one is worse than no switch: an extension
//! that genuinely needs a build step fails, and the control that looks like the
//! fix is inert.
//!
//! # Deps land in place, and the hash is pinned after
//!
//! The download already swapped the folder into its final location and no
//! worker can spawn against a `downloaded` extension, so there is no race and
//! no second swap. `node_modules` and `deno.lock` are excluded from the content
//! hash, so pinning after `deno install` gives the same digest as the pristine
//! downloaded content.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{AppError, ErrorCode, Result};

/// The `deno install` invocation for one extension folder.
///
/// A struct rather than a spawn so the whole decision surface can be asserted
/// without a process, which is the only way the missing flag above is testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepsInstallPlan {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// The complete environment. Cleared and rebuilt, not inherited: an install
    /// runs third-party build scripts' resolution logic, and the core's
    /// environment can hold an operator's API keys.
    pub env: Vec<(OsString, OsString)>,
}

/// Build the invocation.
///
/// `--node-modules-dir=auto` lands `node_modules` in the extension folder
/// without editing the shipped `deno.json`, and `DENO_DIR` keeps the package
/// cache under `~/.loca` rather than in the user's home cache.
/// `allow_scripts` is [`allow_postinstall_scripts`]'s answer, passed in rather
/// than read so this stays one pure function over its whole decision surface.
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

/// Whether npm lifecycle scripts may run during `deno install`.
///
/// `extensions.ignorePostinstallScripts` is phrased as BLOCKING, so the flag is
/// its inverse. It defaults to on, and the fallback here says so too: a record
/// that failed to resolve must land on blocked, never on allowed, because the
/// two failures are not symmetric. Blocked wrongly is an install that fails
/// loudly; allowed wrongly is third-party code running with the user's
/// privileges during what they asked for as an install.
pub fn allow_postinstall_scripts(settings: &crate::settings::SettingsRecord) -> bool {
    !crate::settings::access::bool_setting(settings, "extensions.ignorePostinstallScripts", true)
}

/// Does this extension declare dependencies at all?
///
/// An unparseable config is an ERROR rather than a "no": silently skipping a
/// broken `deno.json` would install nothing and then leave the extension
/// looking installed, and its first tool call would fail on a missing import
/// with no connection to the install that never happened.
pub async fn has_declared_deps(folder: &Path) -> Result<bool> {
    if let Some(text) = read_optional(&folder.join("deno.json")).await {
        let config: Value = serde_json::from_str(&text).map_err(|_| {
            AppError::new(
                ErrorCode::DepsInstallFailed,
                format!("unparseable deno.json in {}", folder.display()),
            )
        })?;
        if let Some(imports) = config.get("imports").and_then(Value::as_object) {
            // Only `npm:` and `jsr:` specifiers need installing. A bare
            // relative or `https:` import resolves without one, so treating
            // every import as a dependency would run `deno install` for every
            // extension and make the runtime a hard requirement for all of them.
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

/// The tail of a failed install's stderr, for the error the user sees.
///
/// Bounded at twelve non-blank lines: `deno install` can print hundreds, and
/// the useful part is at the end. An unbounded message would reach the client
/// as a wall of text nobody reads, and the first line is rarely the reason.
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
        // The most important assertion in this file, and it is about something
        // that is NOT there. `--allow-scripts` makes more packages install
        // successfully, so nothing about its presence looks wrong; what it
        // actually does is execute whatever a transitive dependency's author put
        // in `postinstall`, with the user's privileges, during what the user
        // asked for as an install.
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

    /// The setting is phrased as BLOCKING, so the flag is its inverse, and
    /// getting that backwards is the one mistake here that fails open.
    ///
    /// The absent case is asserted separately from the explicit `true`: they are
    /// the same answer for different reasons, and a reader that returned the
    /// fallback unconditionally would pass on one of them alone.
    #[test]
    fn an_unset_or_hostile_record_lands_on_blocked() {
        assert!(!allow_postinstall_scripts(&record(serde_json::json!({}))));
        assert!(!allow_postinstall_scripts(&record(
            serde_json::json!({ "extensions.ignorePostinstallScripts": true })
        )));
        assert!(!allow_postinstall_scripts(&record(
            serde_json::json!({ "extensions.ignorePostinstallScripts": "yes please" })
        )));
        // And the one value that opens it, which is the user saying so.
        assert!(allow_postinstall_scripts(&record(
            serde_json::json!({ "extensions.ignorePostinstallScripts": false })
        )));
    }

    #[test]
    fn the_package_cache_stays_under_the_core_home() {
        // Without `DENO_DIR` the cache lands in the user's own Deno cache,
        // where uninstalling Loca leaves it behind and a channel switch shares
        // it with the other channel.
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
        // An install resolves third-party packages and can run their
        // resolution logic. The core's environment can hold an operator's
        // OPENAI_API_KEY, and nothing about an install needs it.
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
        // Nothing at all.
        assert!(!has_declared_deps(dir.path()).await.unwrap());

        // A deno.json with no imports, and one with imports that need no
        // install. Treating these as dependencies would make the `deno` binary
        // a hard requirement for every extension, including pure-TypeScript
        // ones that never need it.
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

        // An empty `dependencies` is not a dependency either.
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
        // Silently skipping would install nothing, mark the extension
        // installed, and leave its first tool call failing on a missing import
        // with no connection to the install that never happened.
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

        // Blank lines are dropped BEFORE the bound, or a run of them eats the
        // whole budget and the message carries nothing.
        let padded: Vec<String> = std::iter::repeat_n(String::new(), 30)
            .chain(["the real reason".to_string()])
            .collect();
        assert_eq!(stderr_tail(&padded), "the real reason");

        assert_eq!(stderr_tail(&[]), "");
        assert!(deps_failed("ext", 1, "").message.ends_with("for ext"));
        assert!(deps_failed("ext", 1, "boom").message.ends_with(":\nboom"));
    }
}
