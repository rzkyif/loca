//! The Deno sandbox derivation: which `--allow-*` flags a tool worker is
//! spawned with, and which paths it is denied whatever it was granted.
//!
//! A declaration contributes only when it is in the tool's `required` list AND
//! a `granted` grant row names its exact key, so a stale grant row from an older
//! `loca.json` is inert. Three refusals are security controls: a write grant
//! into the extension's own install directory (TOCTOU), an empty path expansion
//! (`--allow-read=` is a broader grant to Deno, not none), and `$env.VAR` outside
//! [`PATH_ENV_ALLOWLIST`] (a secret would land in `ps` output).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::contain::is_within;
use crate::contract::api::{
    Grant, GrantState, PermissionDecl, PermissionDeclPortsItem, PermissionKind,
};
use crate::paths::Paths;
use crate::secrets;

/// The closed list of path-shaped, non-secret variables `$env.VAR` may name.
const PATH_ENV_ALLOWLIST: &[&str] = &[
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TMPDIR",
    "TMP",
    "TEMP",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
];

/// Uppercase and underscore only, so a lowercase variable is never a candidate.
#[allow(clippy::unwrap_used)]
fn env_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$env\.([A-Z_][A-Z0-9_]*)").unwrap())
}

/// How `$env.VAR` resolves; injected so the derivation is a pure function.
pub trait EnvLookup {
    fn get(&self, key: &str) -> Option<String>;
}

/// The real process environment.
pub struct ProcessEnv;

impl EnvLookup for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

impl EnvLookup for BTreeMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        BTreeMap::get(self, key).cloned()
    }
}

/// The `$`-template substitutions for a declared read/write path.
#[derive(Debug, Clone)]
pub struct PathTemplates {
    pub home: PathBuf,
    pub downloads: PathBuf,
    pub models: PathBuf,
    pub sessions: PathBuf,
    pub extension: PathBuf,
}

impl PathTemplates {
    /// An unresolvable `$home` is an EMPTY path (which then drops), never a
    /// guess: `/` would turn it into a root-wide grant.
    pub fn for_extension(paths: &Paths, extension: PathBuf, env: &dyn EnvLookup) -> Self {
        let home = home_dir(env);
        Self {
            downloads: default_downloads_dir(&home),
            home,
            models: paths.models_dir().to_path_buf(),
            sessions: paths.sessions_dir(),
            extension,
        }
    }
}

fn home_dir(env: &dyn EnvLookup) -> PathBuf {
    let value = env
        .get("HOME")
        .or_else(|| env.get("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(value)
}

fn default_downloads_dir(home: &Path) -> PathBuf {
    if home.as_os_str().is_empty() {
        return PathBuf::new();
    }
    home.join("Downloads")
}

/// One tool's declarations and the grant rows recorded against them.
#[derive(Debug, Clone, Default)]
pub struct ToolGrants {
    pub required: Vec<PermissionDecl>,
    pub grants: Vec<Grant>,
}

/// The derived permission set. `BTreeSet` so the comma lists are stable per run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlagSet {
    /// The bare `--allow-net`, set by a wildcard-host grant. Supersedes `net`.
    net_all: bool,
    net: BTreeSet<String>,
    read: BTreeSet<String>,
    write: BTreeSet<String>,
    run: BTreeSet<String>,
    env: BTreeSet<String>,
    ffi: bool,
    sys: BTreeSet<String>,
}

impl FlagSet {
    /// The grants worth telling an operator about (informational only): those
    /// leaving the sandbox (`ffi`, `run`) or covering the home directory itself.
    pub fn elevated(&self, home: &Path) -> Vec<String> {
        let mut out = Vec::new();
        if self.ffi {
            out.push("ffi".to_string());
        }
        if !self.run.is_empty() {
            out.push(format!("run({})", join(&self.run)));
        }
        if !home.as_os_str().is_empty() {
            let home = home.to_string_lossy();
            let with_slash = format!("{home}/");
            for path in self.read.iter().chain(self.write.iter()) {
                if path.as_str() == home || path.as_str() == with_slash {
                    out.push(format!("home-wide({path})"));
                }
            }
        }
        out
    }
}

/// The union of the granted declarations across the tools passed in. The pool
/// passes exactly one, so a tool never runs with a sibling's `run` or `ffi`.
pub fn union_flags(
    tools: &[ToolGrants],
    templates: &PathTemplates,
    env: &dyn EnvLookup,
) -> FlagSet {
    let mut out = FlagSet::default();
    for tool in tools {
        let granted: BTreeSet<&str> = tool
            .grants
            .iter()
            .filter(|g| matches!(g.state, GrantState::Granted))
            .map(|g| g.permission_key.as_str())
            .collect();
        for decl in &tool.required {
            if !granted.contains(permission_key(decl).as_str()) {
                continue;
            }
            apply_decl(&mut out, decl, templates, env);
        }
    }
    out
}

/// Render the flag set as Deno arguments. Every branch tests emptiness: an empty
/// set must emit NO flag, since `--allow-read=` is a broader grant than any
/// author declared (and the schema's `.default([])` erases presence anyway).
pub fn flag_set_to_args(flags: &FlagSet) -> Vec<String> {
    let mut args = Vec::new();
    if !flags.read.is_empty() {
        args.push(format!("--allow-read={}", join(&flags.read)));
    }
    if !flags.write.is_empty() {
        args.push(format!("--allow-write={}", join(&flags.write)));
    }
    // Deno cannot express "any host, only these ports", so the bare flag
    // supersedes the entries.
    if flags.net_all {
        args.push("--allow-net".to_string());
    } else if !flags.net.is_empty() {
        args.push(format!("--allow-net={}", join(&flags.net)));
    }
    if !flags.env.is_empty() {
        args.push(format!("--allow-env={}", join(&flags.env)));
    }
    if !flags.run.is_empty() {
        args.push(format!("--allow-run={}", join(&flags.run)));
    }
    if flags.ffi {
        args.push("--allow-ffi".to_string());
    }
    if !flags.sys.is_empty() {
        args.push(format!("--allow-sys={}", join(&flags.sys)));
    }
    args
}

fn join(set: &BTreeSet<String>) -> String {
    set.iter().cloned().collect::<Vec<_>>().join(",")
}

/// Expand the `$`-templates in a declared path, or `None` when the result must
/// not become a grant: the expansion is empty, a substituted template resolved
/// to nothing (`$home/notes` with no home would land at `/notes`), or it
/// contains a comma (Deno's flag lists have no escape, so it would split into
/// two grants).
pub fn expand_path(
    template: &str,
    templates: &PathTemplates,
    env: &dyn EnvLookup,
) -> Option<String> {
    let mut lost_a_template = false;
    let mut substitute = |haystack: String, needle: &str, value: &Path| -> String {
        if !haystack.contains(needle) {
            return haystack;
        }
        let value = lossy(value);
        lost_a_template |= value.is_empty();
        haystack.replace(needle, &value)
    };
    let expanded = substitute(template.to_string(), "$home", &templates.home);
    let expanded = substitute(expanded, "$downloads", &templates.downloads);
    let expanded = substitute(expanded, "$models", &templates.models);
    let expanded = substitute(expanded, "$sessions", &templates.sessions);
    let expanded = substitute(expanded, "$extension", &templates.extension);
    let expanded = env_ref_re().replace_all(&expanded, |caps: &regex::Captures<'_>| {
        let key = &caps[1];
        let value = if PATH_ENV_ALLOWLIST.contains(&key) {
            env.get(key).unwrap_or_default()
        } else {
            String::new()
        };
        lost_a_template |= value.is_empty();
        value
    });
    if lost_a_template || expanded.is_empty() || expanded.contains(',') {
        None
    } else {
        Some(expanded.into_owned())
    }
}

/// The first core secret whose path contains a comma, if any. One comma above
/// `~/.loca` truncates the ENTIRE deny list at render time, so boot refuses
/// rather than spawning children with the vault undenied.
pub fn unrepresentable_deny_path(paths: &Paths) -> Option<PathBuf> {
    core_secret_deny_paths(paths)
        .into_iter()
        .find(|p| p.to_string_lossy().contains(','))
}

fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn apply_decl(
    flags: &mut FlagSet,
    decl: &PermissionDecl,
    templates: &PathTemplates,
    env: &dyn EnvLookup,
) {
    match decl {
        PermissionDecl::Net { host, ports, .. } => {
            // Deno has no "*" token: a wildcard host is the bare `--allow-net`
            // and a wildcard port is the bare host. Emitting "*" kills the
            // worker at spawn.
            if host == "*" {
                flags.net_all = true;
                return;
            }
            for port in ports {
                flags.net.insert(match port {
                    PermissionDeclPortsItem::Variant1(_) => host.clone(),
                    PermissionDeclPortsItem::Variant0(port) => format!("{host}:{port}"),
                });
            }
        }
        PermissionDecl::Read { path, .. } => match expand_path(path, templates, env) {
            Some(expanded) => {
                flags.read.insert(expanded);
            }
            None => tracing::warn!(
                target: "loca::extensions",
                "dropping read permission with empty path expansion: \"{path}\""
            ),
        },
        PermissionDecl::Write { path, .. } => {
            let Some(expanded) = expand_path(path, templates, env) else {
                tracing::warn!(
                    target: "loca::extensions",
                    "dropping write permission with empty path expansion: \"{path}\""
                );
                return;
            };
            // TOCTOU refusal: a write grant into the install dir lets a tool
            // rewrite its own entry file between the hash verify and the
            // worker's import, so the gate would attest one file and run another.
            if is_within(&templates.extension, Path::new(&expanded)) {
                tracing::warn!(
                    target: "loca::extensions",
                    "dropping self-write grant into the extension install dir ($extension): \"{path}\""
                );
                return;
            }
            flags.write.insert(expanded);
        }
        PermissionDecl::Run { binary, .. } => {
            flags.run.insert(binary.clone());
        }
        PermissionDecl::Env { key, .. } => {
            flags.env.insert(key.clone());
        }
        PermissionDecl::Ffi { .. } => flags.ffi = true,
        PermissionDecl::Sys { flag, .. } => {
            flags.sys.insert(flag.clone());
        }
        // Module-broker permissions carry no sandbox flag; the broker enforces
        // them per request.
        PermissionDecl::Memories { .. }
        | PermissionDecl::Llm { .. }
        | PermissionDecl::Tts { .. }
        | PermissionDecl::Stt { .. } => {}
    }
}

/// The stable identity of a declaration and the key a grant row is recorded
/// against. Persisted in `grants` and on the wire, so the format is fixed.
pub fn permission_key(decl: &PermissionDecl) -> String {
    match decl {
        PermissionDecl::Net { host, ports, .. } => {
            let ports = ports.iter().map(port_str).collect::<Vec<_>>().join(",");
            format!("net:{host}:{ports}")
        }
        PermissionDecl::Read { path, .. } => format!("read:{path}"),
        PermissionDecl::Write { path, .. } => format!("write:{path}"),
        PermissionDecl::Run { binary, .. } => format!("run:{binary}"),
        PermissionDecl::Env { key, .. } => format!("env:{key}"),
        PermissionDecl::Ffi { .. } => "ffi".to_string(),
        PermissionDecl::Sys { flag, .. } => format!("sys:{flag}"),
        PermissionDecl::Memories { access, .. } => format!("memories:{access}"),
        PermissionDecl::Llm { .. } => "llm".to_string(),
        PermissionDecl::Tts { .. } => "tts".to_string(),
        PermissionDecl::Stt { .. } => "stt".to_string(),
    }
}

pub(crate) fn port_str(port: &PermissionDeclPortsItem) -> String {
    match port {
        PermissionDeclPortsItem::Variant0(n) => n.to_string(),
        PermissionDeclPortsItem::Variant1(_) => "*".to_string(),
    }
}

/// The declaration's kind, as the database column and the wire spell it.
pub fn permission_kind_of(decl: &PermissionDecl) -> PermissionKind {
    match decl {
        PermissionDecl::Net { .. } => PermissionKind::Net,
        PermissionDecl::Read { .. } => PermissionKind::Read,
        PermissionDecl::Write { .. } => PermissionKind::Write,
        PermissionDecl::Run { .. } => PermissionKind::Run,
        PermissionDecl::Env { .. } => PermissionKind::Env,
        PermissionDecl::Ffi { .. } => PermissionKind::Ffi,
        PermissionDecl::Sys { .. } => PermissionKind::Sys,
        PermissionDecl::Memories { .. } => PermissionKind::Memories,
        PermissionDecl::Llm { .. } => PermissionKind::Llm,
        PermissionDecl::Tts { .. } => PermissionKind::Tts,
        PermissionDecl::Stt { .. } => PermissionKind::Stt,
    }
}

/// The core's private paths a sandboxed child must never touch even under a
/// broad grant: `--deny-*` beats any `--allow-*`. Enumerated rather than a
/// blanket root deny, since `$sessions` lives under the root and may be granted.
pub fn core_secret_deny_paths(paths: &Paths) -> Vec<PathBuf> {
    let root = paths.root();
    let db = paths.db_file();
    vec![
        paths.secrets_enc_file(),
        with_suffix(&paths.secrets_enc_file(), ".tmp"),
        // Declared but unused; kept denied on installs old enough to have one.
        paths.secrets_plain_file(),
        secrets::master_key_file(root),
        secrets::master_key_tmp_file(root),
        paths.admin_token_file(),
        paths.admin_password_file(),
        db.clone(),
        with_suffix(&db, "-wal"),
        with_suffix(&db, "-shm"),
        with_suffix(&db, "-journal"),
        // Extension databases and the memory store are reached only through
        // the broker, so a child never needs disk access to them.
        paths.extension_data_root(),
        paths.memories_dir(),
    ]
}

/// The deny list as the two flags every sandboxed child is spawned with.
pub fn deny_args(paths: &Paths) -> Vec<String> {
    let list = core_secret_deny_paths(paths)
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(",");
    vec![
        format!("--deny-read={list}"),
        format!("--deny-write={list}"),
    ]
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::contract::api::PermissionDeclAccess;

    fn templates() -> PathTemplates {
        PathTemplates {
            home: PathBuf::from("/home/u"),
            downloads: PathBuf::from("/home/u/Downloads"),
            models: PathBuf::from("/home/u/.loca/models"),
            sessions: PathBuf::from("/home/u/.loca/stable/core/sessions"),
            extension: PathBuf::from("/home/u/.loca/stable/core/extensions/demo"),
        }
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn read(path: &str) -> PermissionDecl {
        PermissionDecl::Read {
            optional: None,
            path: path.to_string(),
            reason: "r".to_string(),
        }
    }

    fn write(path: &str) -> PermissionDecl {
        PermissionDecl::Write {
            optional: None,
            path: path.to_string(),
            reason: "w".to_string(),
        }
    }

    fn net(host: &str, ports: &[PermissionDeclPortsItem]) -> PermissionDecl {
        PermissionDecl::Net {
            host: host.to_string(),
            optional: None,
            ports: ports.to_vec(),
            reason: "n".to_string(),
        }
    }

    fn port(n: i64) -> PermissionDeclPortsItem {
        PermissionDeclPortsItem::Variant0(n)
    }

    fn any_port() -> PermissionDeclPortsItem {
        PermissionDeclPortsItem::Variant1(crate::contract::api::PermissionDeclPortsItemVariant1::X)
    }

    fn run(binary: &str) -> PermissionDecl {
        PermissionDecl::Run {
            binary: binary.to_string(),
            optional: None,
            reason: "x".to_string(),
        }
    }

    fn env_decl(key: &str) -> PermissionDecl {
        PermissionDecl::Env {
            key: key.to_string(),
            optional: None,
            reason: String::new(),
        }
    }

    fn ffi() -> PermissionDecl {
        PermissionDecl::Ffi {
            optional: None,
            reason: String::new(),
        }
    }

    fn sys(flag: &str) -> PermissionDecl {
        PermissionDecl::Sys {
            flag: flag.to_string(),
            optional: None,
            reason: String::new(),
        }
    }

    fn memories(access: PermissionDeclAccess) -> PermissionDecl {
        PermissionDecl::Memories {
            access,
            optional: None,
            reason: String::new(),
        }
    }

    fn llm() -> PermissionDecl {
        PermissionDecl::Llm {
            optional: None,
            reason: String::new(),
        }
    }

    fn tts() -> PermissionDecl {
        PermissionDecl::Tts {
            optional: None,
            reason: String::new(),
        }
    }

    fn stt() -> PermissionDecl {
        PermissionDecl::Stt {
            optional: None,
            reason: String::new(),
        }
    }

    fn grant(decl: &PermissionDecl, state: GrantState) -> Grant {
        Grant {
            granted_at_ms: 0,
            permission_key: permission_key(decl),
            permission_kind: permission_kind_of(decl),
            state,
            tool_id: "t1".to_string(),
        }
    }

    fn all_granted(decls: &[PermissionDecl]) -> ToolGrants {
        ToolGrants {
            required: decls.to_vec(),
            grants: decls
                .iter()
                .map(|d| grant(d, GrantState::Granted))
                .collect(),
        }
    }

    fn args_for(decls: &[PermissionDecl], env_pairs: &[(&str, &str)]) -> Vec<String> {
        let flags = union_flags(&[all_granted(decls)], &templates(), &env(env_pairs));
        flag_set_to_args(&flags)
    }

    #[test]
    fn a_comma_in_the_core_root_is_reported_rather_than_silently_truncating_the_deny_list() {
        // Asserted through the RENDERED flag: the path list is correct, the
        // flag built from it is not.
        let paths = Paths::rooted("/tmp/a,b");
        let args = deny_args(&paths);
        let vault = paths.secrets_enc_file().to_string_lossy().into_owned();
        let denied: Vec<&str> = args
            .iter()
            .flat_map(|a| {
                a.split_once('=')
                    .map(|(_, v)| v)
                    .unwrap_or_default()
                    .split(',')
            })
            .collect();
        assert!(
            !denied.contains(&vault.as_str()),
            "this test's premise is gone: the vault path survived the comma join, so the \
             truncation it guards no longer happens and the guard can go",
        );
        assert!(
            unrepresentable_deny_path(&paths).is_some(),
            "a core root with a comma was accepted, so the core would boot with its sandbox \
             deny list expanded into paths that are not the secrets it is meant to protect",
        );
        assert!(
            unrepresentable_deny_path(&Paths::rooted("/tmp/plain")).is_none(),
            "an ordinary root was rejected, which would refuse to boot on every machine",
        );
    }

    #[test]
    fn a_granted_path_containing_a_comma_is_dropped_rather_than_split_into_two_grants() {
        let commad = env(&[("HOME", "/home/a,b")]);
        let templates = PathTemplates::for_extension(
            &Paths::rooted("/root"),
            PathBuf::from("/root/extensions/demo"),
            &commad,
        );
        assert_eq!(expand_path("$home/notes", &templates, &commad), None);
        let clean = env(&[("HOME", "/home/ab")]);
        let templates =
            PathTemplates::for_extension(&Paths::rooted("/root"), PathBuf::from("/x"), &clean);
        assert_eq!(
            expand_path("$home/notes", &templates, &clean).as_deref(),
            Some("/home/ab/notes"),
        );
    }

    #[test]
    fn the_templates_come_from_the_environment_and_an_unresolvable_home_is_empty() {
        let paths = Paths::rooted("/root");
        let folder = PathBuf::from("/root/extensions/demo");

        let resolved =
            PathTemplates::for_extension(&paths, folder.clone(), &env(&[("HOME", "/home/u")]));
        assert_eq!(resolved.home, PathBuf::from("/home/u"));
        assert_eq!(resolved.downloads, PathBuf::from("/home/u/Downloads"));
        assert_eq!(resolved.models, paths.models_dir());
        assert_eq!(resolved.sessions, paths.sessions_dir());
        assert_eq!(resolved.extension, folder);

        let windows = PathTemplates::for_extension(
            &paths,
            folder.clone(),
            &env(&[("USERPROFILE", "C:/Users/u")]),
        );
        assert_eq!(windows.home, PathBuf::from("C:/Users/u"));

        let nowhere = PathTemplates::for_extension(&paths, folder, &env(&[]));
        assert_eq!(nowhere.home, PathBuf::new());
        assert_eq!(nowhere.downloads, PathBuf::new());
        assert_eq!(expand_path("$home", &nowhere, &env(&[])), None);
        // `"" + "/notes"` is not empty, so a whole-result check would grant
        // one component off the filesystem root.
        assert_eq!(
            expand_path("$home/notes", &nowhere, &env(&[])),
            None,
            "an unresolvable home reached a flag",
        );
        assert_eq!(
            expand_path("$home/notes", &resolved, &env(&[("HOME", "/home/u")])),
            Some("/home/u/notes".to_string()),
        );
    }

    #[test]
    fn the_elevated_grants_are_the_ones_that_leave_the_sandbox_or_cover_a_home() {
        let home = PathBuf::from("/home/u");
        let mut flags = FlagSet::default();
        assert!(
            flags.elevated(&home).is_empty(),
            "a bare set is not elevated"
        );

        flags.read.insert("/home/u/notes".to_string());
        flags.write.insert("/tmp/scratch".to_string());
        assert!(
            flags.elevated(&home).is_empty(),
            "an ordinary path was reported as elevated: {:?}",
            flags.elevated(&home)
        );

        flags.ffi = true;
        flags.run.insert("ffmpeg".to_string());
        flags.read.insert("/home/u".to_string());
        flags.write.insert("/home/u/".to_string());
        let elevated = flags.elevated(&home);
        assert_eq!(
            elevated,
            vec![
                "ffi".to_string(),
                "run(ffmpeg)".to_string(),
                "home-wide(/home/u)".to_string(),
                "home-wide(/home/u/)".to_string(),
            ],
            "the elevated set drifted"
        );

        // An empty home must not make every path look home-wide.
        let mut empty_home = FlagSet::default();
        empty_home.read.insert(String::new());
        assert!(empty_home.elevated(&PathBuf::new()).is_empty());
    }

    #[test]
    fn an_empty_permission_set_emits_no_flag_at_all_rather_than_an_empty_one() {
        assert!(flag_set_to_args(&FlagSet::default()).is_empty());
        assert!(args_for(&[], &[]).is_empty());
        // Positive control: with something granted, flags DO appear.
        assert_eq!(
            args_for(&[read("/tmp/x")], &[]),
            vec!["--allow-read=/tmp/x".to_string()]
        );
    }

    #[test]
    fn a_write_grant_into_the_install_dir_is_refused() {
        // The exact directory is the case a `starts_with(root + sep)` loses.
        for path in ["$extension", "$extension/sub/f.txt", "$extension/"] {
            let args = args_for(&[write(path)], &[]);
            assert!(
                args.is_empty(),
                "expected no flag for a self-write grant on {path}, got {args:?}"
            );
        }
    }

    #[test]
    fn a_write_grant_that_climbs_back_into_the_install_dir_is_refused() {
        for path in [
            "$extension/../demo",
            "$extension/sub/../../demo/x",
            "/home/u/.loca/stable/core/extensions/../extensions/demo/x",
        ] {
            let args = args_for(&[write(path)], &[]);
            assert!(
                args.is_empty(),
                "expected no flag for a climbing self-write grant on {path}, got {args:?}"
            );
        }
    }

    #[test]
    fn a_sibling_of_the_install_dir_is_still_writable() {
        // Component-wise, not a textual prefix: `demo-2` is another extension.
        let args = args_for(&[write("$extension-2/out.txt")], &[]);
        assert_eq!(
            args,
            vec!["--allow-write=/home/u/.loca/stable/core/extensions/demo-2/out.txt"]
        );
    }

    #[test]
    fn a_read_grant_into_the_install_dir_is_allowed() {
        let args = args_for(&[read("$extension/data.json")], &[]);
        assert_eq!(
            args,
            vec!["--allow-read=/home/u/.loca/stable/core/extensions/demo/data.json"]
        );
    }

    #[test]
    fn an_empty_path_expansion_never_reaches_the_flag_list() {
        let args = args_for(&[read("$env.TMPDIR"), write("$env.TMPDIR")], &[]);
        assert!(args.is_empty(), "expected no flags at all, got {args:?}");
        assert!(
            !args.iter().any(|a| a.ends_with('=')),
            "an empty-valued permission flag reached the args: {args:?}"
        );
    }

    #[test]
    fn a_resolvable_env_var_does_reach_the_flag_list() {
        let args = args_for(&[read("$env.TMPDIR/cache")], &[("TMPDIR", "/var/tmp")]);
        assert_eq!(args, vec!["--allow-read=/var/tmp/cache"]);
    }

    #[test]
    fn an_off_allowlist_env_var_resolves_to_nothing_and_takes_its_declaration_with_it() {
        // `$env.OPENAI_API_KEY/x` must not become `/x`.
        let args = args_for(
            &[read("$env.OPENAI_API_KEY"), read("$env.OPENAI_API_KEY/x")],
            &[("OPENAI_API_KEY", "sk-secret-value")],
        );
        assert!(args.is_empty(), "expected no flags at all, got {args:?}");
        assert!(
            !args.iter().any(|a| a.contains("sk-secret-value")),
            "a non-path environment value reached the flag list: {args:?}"
        );
    }

    #[test]
    fn a_lowercase_env_reference_is_not_even_a_candidate() {
        let args = args_for(&[read("/tmp/$env.home")], &[("home", "/home/u")]);
        assert_eq!(args, vec!["--allow-read=/tmp/$env.home"]);
    }

    #[test]
    fn a_wildcard_host_emits_the_bare_allow_net_and_nothing_else() {
        let args = args_for(
            &[net("*", &[port(80)]), net("example.com", &[port(443)])],
            &[],
        );
        assert!(args.contains(&"--allow-net".to_string()), "got {args:?}");
        assert_eq!(
            args.iter().filter(|a| a.starts_with("--allow-net")).count(),
            1,
            "the bare flag must SUPERSEDE the entries, not accompany them: {args:?}"
        );
    }

    #[test]
    fn a_wildcard_port_emits_the_bare_host() {
        let args = args_for(&[net("example.com", &[any_port()])], &[]);
        assert_eq!(args, vec!["--allow-net=example.com"]);
    }

    #[test]
    fn net_entries_merge_into_one_flag() {
        let args = args_for(
            &[
                net("a.example", &[port(443), port(8443)]),
                net("b.example", &[port(443)]),
            ],
            &[],
        );
        assert_eq!(
            args,
            vec!["--allow-net=a.example:443,a.example:8443,b.example:443"]
        );
    }

    #[test]
    fn only_granted_declarations_contribute() {
        for state in [GrantState::Ask, GrantState::Denied] {
            let decl = read("/tmp/x");
            let tool = ToolGrants {
                required: vec![decl.clone()],
                grants: vec![grant(&decl, state)],
            };
            let args = flag_set_to_args(&union_flags(&[tool], &templates(), &env(&[])));
            assert!(args.is_empty(), "state {state} contributed {args:?}");
        }
    }

    #[test]
    fn a_grant_enables_only_the_declaration_whose_key_it_names() {
        // `/tmp/ungranted` has no grant of its own, so a match on kind or
        // prefix rather than the exact key would hand it the sibling's grant.
        let tool = ToolGrants {
            required: vec![read("/tmp/declared"), read("/tmp/ungranted")],
            grants: vec![
                grant(&read("/tmp/declared"), GrantState::Granted),
                grant(&read("/tmp/stale"), GrantState::Granted),
                grant(&run("/bin/sh"), GrantState::Granted),
            ],
        };
        let args = flag_set_to_args(&union_flags(&[tool], &templates(), &env(&[])));
        assert_eq!(args, vec!["--allow-read=/tmp/declared"]);
    }

    #[test]
    fn the_union_spans_every_tool_passed_in() {
        let a = all_granted(&[read("/tmp/a")]);
        let b = all_granted(&[read("/tmp/b"), run("ffmpeg")]);
        let args = flag_set_to_args(&union_flags(&[a, b], &templates(), &env(&[])));
        assert_eq!(
            args,
            vec!["--allow-read=/tmp/a,/tmp/b", "--allow-run=ffmpeg"]
        );
    }

    #[test]
    fn module_broker_permissions_carry_no_sandbox_flag() {
        let decls = [llm(), tts(), stt(), memories(PermissionDeclAccess::Write)];
        assert!(args_for(&decls, &[]).is_empty());
    }

    #[test]
    fn every_flag_kind_renders_in_the_expected_order() {
        let decls = [
            read("/r"),
            write("/w"),
            net("h", &[port(1)]),
            env_decl("K"),
            run("b"),
            ffi(),
            sys("hostname"),
        ];
        assert_eq!(
            args_for(&decls, &[]),
            vec![
                "--allow-read=/r",
                "--allow-write=/w",
                "--allow-net=h:1",
                "--allow-env=K",
                "--allow-run=b",
                "--allow-ffi",
                "--allow-sys=hostname",
            ]
        );
    }

    #[test]
    fn permission_keys_match_the_persisted_strings() {
        let cases: Vec<(PermissionDecl, &str)> = vec![
            (
                net("example.com", &[port(80), any_port()]),
                "net:example.com:80,*",
            ),
            (read("$home/x"), "read:$home/x"),
            (write("$home/x"), "write:$home/x"),
            (run("ffmpeg"), "run:ffmpeg"),
            (env_decl("PATH"), "env:PATH"),
            (ffi(), "ffi"),
            (sys("hostname"), "sys:hostname"),
            (memories(PermissionDeclAccess::Read), "memories:read"),
            (llm(), "llm"),
            (tts(), "tts"),
            (stt(), "stt"),
        ];
        for (decl, want) in cases {
            assert_eq!(permission_key(&decl), want);
        }
    }

    #[test]
    fn the_deny_list_names_every_core_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(tmp.path());
        let got: BTreeSet<PathBuf> = core_secret_deny_paths(&paths).into_iter().collect();
        let root = tmp.path();
        let want: BTreeSet<PathBuf> = [
            root.join("secrets.enc"),
            root.join("secrets.enc.tmp"),
            root.join("secrets.json"),
            root.join(".master-key"),
            root.join(".master-key.tmp"),
            root.join(".admin-token"),
            root.join(".admin-password"),
            root.join("core.sqlite"),
            root.join("core.sqlite-wal"),
            root.join("core.sqlite-shm"),
            root.join("core.sqlite-journal"),
            root.join("extension-data"),
            root.join("memories"),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn both_deny_flags_carry_the_whole_list() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(tmp.path());
        let args = deny_args(&paths);
        assert_eq!(args.len(), 2);
        for (arg, prefix) in args.iter().zip(["--deny-read=", "--deny-write="]) {
            assert!(arg.starts_with(prefix), "expected {prefix} in {arg}");
            for path in core_secret_deny_paths(&paths) {
                assert!(
                    arg.contains(&path.to_string_lossy().into_owned()),
                    "{prefix} is missing {}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn is_within_answers_the_four_cases_that_matter() {
        let root = Path::new("/a/b");
        assert!(is_within(root, Path::new("/a/b")), "the root itself");
        assert!(is_within(root, Path::new("/a/b/c")), "a descendant");
        assert!(is_within(root, Path::new("/a/b/c/../d")), "a detour inside");
        assert!(!is_within(root, Path::new("/a/bc")), "a textual prefix");
        assert!(!is_within(root, Path::new("/a/b/../c")), "climbs out");
        assert!(!is_within(root, Path::new("/a")), "the parent");
    }
}
