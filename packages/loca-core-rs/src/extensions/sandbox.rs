//! The Deno sandbox derivation: which `--allow-*` flags a tool worker is
//! spawned with, and which paths it is denied whatever it was granted.
//!
//! Ports `packages/loca-core/src/extensions/permissions.ts` and
//! `packages/loca-core/src/shared/sandbox-deny.ts`.
//!
//! # What decides the flag set
//!
//! Two conditions must BOTH hold for a declaration to contribute: the key must
//! appear in the tool's `required` list (parsed from `loca.json`) AND a grant row
//! for that exact key must be in state `granted`. That intersection is what makes
//! a stale grant row left over from a previous `loca.json` version inert: the
//! author's declaration is the vocabulary, the grant is the decision, and a
//! decision about something no longer declared decides nothing.
//!
//! # The three refusals, each a control rather than tidiness
//!
//! 1. **A write grant into the extension's own install directory is refused.**
//!    See [`apply_decl`]. This is a TOCTOU defence, not neatness.
//! 2. **An empty path expansion is dropped, never emitted.** `--allow-read=`
//!    with an empty value is not "no grant": Deno reads it as a far broader
//!    grant than the author declared. [`expand_path`] returns `None` for an
//!    empty expansion so the empty case cannot reach the set by falling through.
//! 3. **`$env.VAR` resolves only against [`PATH_ENV_ALLOWLIST`].** The core
//!    process can hold an operator's API key; resolving it into a granted path
//!    would smuggle its value into a flag, and the flag list is visible in `ps`.
//!
//! # The environment is passed in, never read here
//!
//! `$env.VAR` resolves through an [`EnvLookup`] the caller supplies (D-010: no
//! process globals for state). Production passes [`ProcessEnv`]; tests pass a
//! map. That also removes the `Deno.env.set` / restore dance the TypeScript test
//! needed, which could not run concurrently and which `std::env::set_var` makes
//! outright unsound in a threaded test binary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::contract::api::{
    Grant, GrantState, PermissionDecl, PermissionDeclPortsItem, PermissionKind,
};
use crate::paths::Paths;
use crate::secrets;

/// `$env.VAR` resolves ONLY against this allowlist of path-shaped, non-secret
/// variables. An off-list key resolves to nothing, which then hits the
/// empty-expansion drop, exactly as an unset variable would.
///
/// The list is closed on purpose. A secret that is not a path has no business in
/// a path, and the cost of being wrong here is an operator's credential printed
/// into a process argument list.
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

/// Uppercase and underscore only, so a lowercase environment variable is not
/// even a candidate for expansion. Mirrors `permissions.ts:121`.
// A literal pattern, so a compile failure here is a build-time bug rather than
// anything a running core can hit.
#[allow(clippy::unwrap_used)]
fn env_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$env\.([A-Z_][A-Z0-9_]*)").unwrap())
}

/// How `$env.VAR` resolves. Injected rather than read, so the derivation is a
/// pure function of its inputs.
pub trait EnvLookup {
    fn get(&self, key: &str) -> Option<String>;
}

/// The real process environment. The only implementation production uses.
pub struct ProcessEnv;

impl EnvLookup for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// A fixed environment, which is what every test wants and what a future
/// snapshot-at-boot would use.
impl EnvLookup for BTreeMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        BTreeMap::get(self, key).cloned()
    }
}

/// The template substitutions applied to a declared read/write path at spawn
/// time. `$extension` is the extension's own install directory, and is the one
/// template that is also a refusal (see [`apply_decl`]).
#[derive(Debug, Clone)]
pub struct PathTemplates {
    pub home: PathBuf,
    pub downloads: PathBuf,
    pub models: PathBuf,
    pub sessions: PathBuf,
    pub extension: PathBuf,
}

impl PathTemplates {
    /// The substitutions for one extension on this host.
    ///
    /// An unresolvable `$home` (no `HOME`, no `USERPROFILE`) is an EMPTY path
    /// rather than a guess, so it hits the empty-expansion drop and the
    /// declaration grants nothing. Guessing `/` here would turn an
    /// unresolvable template into a root-wide grant.
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

/// The user's home, as the templates see it. Empty when neither variable is set.
fn home_dir(env: &dyn EnvLookup) -> PathBuf {
    let value = env
        .get("HOME")
        .or_else(|| env.get("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(value)
}

/// `$downloads`, which is the platform default rather than a configured one:
/// nothing in this core lets a user move it, so reading a setting would invent
/// a knob the specification does not have.
///
/// Empty for an empty home, which then drops rather than expanding to
/// `/Downloads`.
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

/// The derived permission set, before it is rendered as flags.
///
/// `BTreeSet` rather than a hash set on purpose: the TypeScript `Set` is
/// insertion-ordered, so `--allow-read=a,b` is stable per run. A hash set would
/// reorder the comma list per process and make every spawn-args assertion flaky
/// in the way that looks like a real bug. Sorted is stable, testable, and Deno
/// does not care about the order.
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
    /// The grants worth telling an operator about, named.
    ///
    /// Informational: every one of these was already approved by the user, and
    /// nothing here refuses anything. It exists because these are the grants
    /// that leave the Deno sandbox (`ffi`, `run`) or cover most of a home
    /// directory, and the only other place they appear is a `ps` listing.
    ///
    /// Empty is the normal answer, so a non-empty one is the signal.
    pub fn elevated(&self, home: &Path) -> Vec<String> {
        let mut out = Vec::new();
        if self.ffi {
            out.push("ffi".to_string());
        }
        if !self.run.is_empty() {
            out.push(format!("run({})", join(&self.run)));
        }
        // A grant of the home directory itself, rather than of something under
        // it. `~/.loca` is denied separately and unconditionally, so this is
        // about the user's documents rather than about Core's secrets.
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

/// The union of the granted declarations across the ENABLED tools passed in.
///
/// The caller decides which tools are in scope. `worker/pool.rs` passes exactly
/// one, the invoked tool, so a benign tool never runs in a process holding a
/// sibling's `run` or `ffi` grant.
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

/// Render the flag set as Deno command-line arguments.
///
/// The three syntax traps this encodes each produce a flag Deno rejects at parse
/// time, which crashes the worker at spawn with no useful diagnostic:
///
/// - a wildcard host is the BARE `--allow-net`, never `--allow-net=*:80`;
/// - a wildcard port is the bare host, never `host:*` (handled in [`apply_decl`]);
/// - the net entries are one merged flag, never two `--allow-net=` flags.
///
/// # Every branch here tests EMPTINESS, and none of them can test presence
///
/// Two independent reasons, and a future editor needs both, because each one
/// alone looks like something that could be tidied.
///
/// **A presence test would be answering a question the data cannot carry.** The
/// eleven `loca.json` permission arrays are declared `.default([])` in the Zod
/// schema, so a tool that declared no `net` permission and one that declared
/// `net: []` arrive here identical. The distinction D-028 makes load-bearing
/// elsewhere was already erased upstream, at parse time, and nothing in the
/// generated type says so. Code that branched on presence would read as though
/// it distinguished the two and would in fact be branching on a constant.
///
/// **And even with the distinction intact, emptiness is the correct test.** An
/// empty set must emit NO flag rather than a flag with an empty value:
/// `--allow-read=` is not "no read grant" to Deno, it is a far broader grant
/// than any author declared. So a declared-but-empty array and an absent one
/// have to produce the same output regardless, and a presence test that emitted
/// the empty-valued flag for the declared-but-empty case would silently widen
/// the sandbox for exactly the tool that was trying to narrow it.
///
/// The same reasoning is why [`expand_path`] returns `Option` and why the
/// caller cannot reach the flag list with an empty value by falling through.
pub fn flag_set_to_args(flags: &FlagSet) -> Vec<String> {
    let mut args = Vec::new();
    if !flags.read.is_empty() {
        args.push(format!("--allow-read={}", join(&flags.read)));
    }
    if !flags.write.is_empty() {
        args.push(format!("--allow-write={}", join(&flags.write)));
    }
    // A wildcard-host grant means "any host". Deno cannot express "any host,
    // only these ports", so the bare flag SUPERSEDES the entries rather than
    // accompanying them.
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

/// Expand the `$`-templates in a declared path.
///
/// Returns `None` when nothing usable came out, and there are two ways for that
/// to happen. Empty is NOT "no grant" to Deno, so the only safe shape is one the
/// caller cannot accidentally emit: every call site either has a path or has
/// nothing.
///
/// 1. **The whole expansion is empty**, which is what an unset or off-allowlist
///    `$env.VAR` alone produces.
/// 2. **A template that was substituted resolved to nothing**, which is the case
///    a whole-result check misses: `$home/notes` on a host with neither `HOME`
///    nor `USERPROFILE` is `"" + "/notes"`, which is not empty and lands the
///    grant at `/notes`, one component off the filesystem ROOT. The declaration
///    said "inside my home"; refusing is the only reading of it that is not a
///    widening.
/// 3. **The expansion contains a comma.** Deno's permission flags are
///    comma-separated with NO escape, so `--allow-read=/home/a,b/notes` is two
///    grants, `/home/a` and `b/notes`, and the first is a whole directory the
///    author never asked for. Same failure direction as the two above: a path
///    that cannot be expressed is dropped rather than expressed wrongly. Rare
///    (a comma in a home directory name) but not hypothetical, and unlike the
///    others it widens by CONSTRUCTION rather than by a missing value.
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

/// The first core secret whose path Deno's flag syntax cannot express, if any.
///
/// Every path in [`core_secret_deny_paths`] derives from the core root, so one
/// comma anywhere above `~/.loca` truncates the ENTIRE deny list rather than one
/// entry: `--deny-read=/a,b/core/secrets.enc,...` denies `/a`, then `b/core/...`,
/// and the real vault, admin token, admin password and SQLite file stop being
/// denied for every tool worker and every deno-runtime MCP server at once. A tool
/// granted `$home` then holds read over the directory containing all of them.
///
/// Returned rather than refused here because the decision belongs at boot: this
/// is a property of the machine's paths, fixed for the process lifetime, so
/// discovering it once at startup is worth more than discovering it on the first
/// tool call. [`expand_path`] handles the other direction, where the untrusted
/// half (a granted path) carries the comma and can simply be dropped.
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
            // Deno has no literal "*" token for a host or a port. A wildcard
            // host means all hosts, which is the bare `--allow-net`; a wildcard
            // port means all ports of that host, which is the bare host with no
            // `:port`. Emitting "*" verbatim produces a flag Deno rejects at
            // parse time, and the worker dies at spawn.
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
            // NEVER grant write into the extension's own install directory.
            //
            // This is a TOCTOU refusal, not tidiness, and it must not be
            // "simplified" away: a write grant here lets a tool rewrite its own
            // entry file in the window between the content-hash verify and the
            // worker's `import`, so the hash gate would attest one file while
            // the worker executed another. The gate's entire premise is that the
            // install directory is immutable while a worker runs.
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
        // Module-broker permissions carry no Deno sandbox flag; the broker
        // enforces them per request.
        PermissionDecl::Memories { .. }
        | PermissionDecl::Llm { .. }
        | PermissionDecl::Tts { .. }
        | PermissionDecl::Stt { .. } => {}
    }
}

/// True when `candidate` resolves inside `root`, or IS `root`.
///
/// Two properties are load-bearing and both are easy to lose in a rewrite:
///
/// - the equal case is `true`, so a grant on `$extension` exactly is refused as
///   well as one on `$extension/sub/f`. A `starts_with(root_with_separator)`
///   port loses it;
/// - `..` is folded BEFORE comparing, so `$extension/../<same-name>` cannot slip
///   through a textual prefix test, and the comparison is component-wise, so
///   `<root>-2/x` is not under `<root>`.
///
/// Deliberately NOT `canonicalize`: a declared write path usually does not exist
/// yet at spawn time, so canonicalising fails and the caller has to decide what
/// the failure means. Failing open is a sandbox escape; failing closed silently
/// drops legitimate grants. Lexical normalisation always answers. The residual
/// gap is a symlink from outside pointing in, which is recorded rather than
/// closed (plan Q3) and needs the attacker to already hold write access to the
/// user's home directory.
use crate::contain::is_within;

/// The stable identity of a declaration, and the key a grant row is recorded
/// against.
///
/// One function where the TypeScript has two identical copies (`permissionKey`
/// in `@loca/shared/domain/extension.ts` and `declKey` in `permissions.ts`).
/// These strings are persisted in the `grants` table and travel on the wire, so
/// the format is fixed by what is already on disk.
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

/// The core's private paths, which a sandboxed child must never read or write
/// even when it holds a broad grant (or `--allow-all`).
///
/// Deno's `--deny-*` takes PRECEDENCE over any `--allow-*`, so this is what
/// keeps a tool granted `$home` (which contains `~/.loca`) out of the vault.
/// Single-sourced here because two sandboxed-child spawn sites consume it, the
/// tool worker and D11's deno-runtime MCP server, and the whole reason
/// `shared/sandbox-deny.ts` exists is that those two must not drift.
///
/// A blanket deny of the root is not usable: sessions live under it and a tool
/// may legitimately be granted `$sessions`. So the sensitive subtrees and files
/// are enumerated, transients included.
pub fn core_secret_deny_paths(paths: &Paths) -> Vec<PathBuf> {
    let root = paths.root();
    let db = paths.db_file();
    vec![
        paths.secrets_enc_file(),
        // The transient write target during a re-encrypt.
        with_suffix(&paths.secrets_enc_file(), ".tmp"),
        // Declared but unused; it survives so the sandbox keeps denying it on an
        // install old enough to still have one.
        paths.secrets_plain_file(),
        secrets::master_key_file(root),
        // D-013: the Rust vault writes the master key atomically, so it has a
        // fixed `.tmp` transient the TypeScript list has no entry for. The
        // constant comes from the secrets module so the two cannot drift.
        secrets::master_key_tmp_file(root),
        paths.admin_token_file(),
        paths.admin_password_file(),
        db.clone(),
        with_suffix(&db, "-wal"),
        with_suffix(&db, "-shm"),
        // The non-WAL fallback journal.
        with_suffix(&db, "-journal"),
        // Every extension's private SQLite database and the memory store are
        // reached ONLY through the core-side module broker, so a sandboxed child
        // never needs filesystem access to either. Denying the whole subtrees is
        // what stops a child granted a broad ancestor (`$home`, which contains
        // `~/.loca`) reading another extension's data off disk.
        paths.extension_data_root(),
        paths.memories_dir(),
    ]
}

/// The deny list rendered as the two flags every sandboxed child is spawned
/// with. Both are needed: `--deny-read` alone leaves the paths writable, and
/// `--deny-write` alone leaves them readable.
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

    fn templates() -> PathTemplates {
        PathTemplates {
            home: PathBuf::from("/home/u"),
            downloads: PathBuf::from("/home/u/Downloads"),
            models: PathBuf::from("/home/u/.loca/models"),
            sessions: PathBuf::from("/home/u/.loca/stable/core/sessions"),
            extension: PathBuf::from("/home/u/.loca/stable/core/extensions/demo"),
        }
    }

    #[test]
    fn a_comma_in_the_core_root_is_reported_rather_than_silently_truncating_the_deny_list() {
        // Deno's permission flags have no escape, so `deny_args` joining on `,`
        // means one comma in the root turns the whole deny list into a set of
        // paths that are not the secrets. Asserted through the RENDERED flag
        // rather than through the path list, because the truncation only exists
        // at rendering: the `Vec<PathBuf>` is perfectly correct and the flag
        // built from it is not.
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

        // Which is why boot refuses instead. Same input, and the refusal names
        // the path so the operator can act on it.
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
        // The other direction, and the untrusted one: the path comes from the
        // extension author's `loca.json` crossed with the operator's `HOME`.
        // `--allow-read=/home/a,b/notes` is TWO grants, and the first is
        // `/home/a`, an entire directory nobody asked for. Dropped for the same
        // reason an empty expansion is dropped: a grant that cannot be
        // expressed must not be expressed approximately.
        let commad = env(&[("HOME", "/home/a,b")]);
        let templates = PathTemplates::for_extension(
            &Paths::rooted("/root"),
            PathBuf::from("/root/extensions/demo"),
            &commad,
        );
        assert_eq!(expand_path("$home/notes", &templates, &commad), None);
        // The control: the identical declaration with a comma-free home is
        // still granted, so the rule is about the comma and not about `$home`.
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
        // An empty `$home` DROPS the declaration (the empty-expansion refusal),
        // where a guessed `/` would turn an unresolvable template into a
        // root-wide grant. Same for `$downloads`, which is derived from it.
        let paths = Paths::rooted("/root");
        let folder = PathBuf::from("/root/extensions/demo");

        let resolved =
            PathTemplates::for_extension(&paths, folder.clone(), &env(&[("HOME", "/home/u")]));
        assert_eq!(resolved.home, PathBuf::from("/home/u"));
        assert_eq!(resolved.downloads, PathBuf::from("/home/u/Downloads"));
        assert_eq!(resolved.models, paths.models_dir());
        assert_eq!(resolved.sessions, paths.sessions_dir());
        assert_eq!(resolved.extension, folder);

        // Windows names it differently, and a core that read only `HOME` would
        // silently grant nothing there.
        let windows = PathTemplates::for_extension(
            &paths,
            folder.clone(),
            &env(&[("USERPROFILE", "C:/Users/u")]),
        );
        assert_eq!(windows.home, PathBuf::from("C:/Users/u"));

        let nowhere = PathTemplates::for_extension(&paths, folder, &env(&[]));
        assert_eq!(nowhere.home, PathBuf::new());
        assert_eq!(nowhere.downloads, PathBuf::new());
        // A bare `$home` drops, because the expansion is empty.
        assert_eq!(expand_path("$home", &nowhere, &env(&[])), None);
        // And so does `$home/notes`, which a whole-result emptiness check
        // misses: `"" + "/notes"` is not empty, so it would land the grant one
        // component off the filesystem ROOT.
        assert_eq!(
            expand_path("$home/notes", &nowhere, &env(&[])),
            None,
            "an unresolvable home reached a flag",
        );
        // The refusal is about the template that was LOST, not about the string
        // that came out: a resolvable home still expands.
        assert_eq!(
            expand_path("$home/notes", &resolved, &env(&[("HOME", "/home/u")])),
            Some("/home/u/notes".to_string()),
        );
    }

    #[test]
    fn the_elevated_grants_are_the_ones_that_leave_the_sandbox_or_cover_a_home() {
        // Informational only, and that is why it is worth a test: nothing
        // refuses on it, so a silently emptied list would never be noticed by
        // anything else.
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

        // An unresolvable home must not make every path look home-wide, which
        // is what an empty-prefix comparison would do.
        let mut empty_home = FlagSet::default();
        empty_home.read.insert(String::new());
        assert!(empty_home.elevated(&PathBuf::new()).is_empty());
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

    fn grant(decl: &PermissionDecl, state: GrantState) -> Grant {
        Grant {
            granted_at_ms: 0,
            permission_key: permission_key(decl),
            permission_kind: permission_kind_of(decl),
            state,
            tool_id: "t1".to_string(),
        }
    }

    /// Every declaration granted, which is the ordinary case.
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
    fn an_empty_permission_set_emits_no_flag_at_all_rather_than_an_empty_one() {
        // The whole rendering branches on EMPTINESS rather than presence, and
        // this is what that has to mean at the output: nothing, not
        // `--allow-read=`. An empty value is a far BROADER grant to Deno than
        // the author asked for, so the failure direction is a widened sandbox
        // for the tool that declared the least.
        //
        // Presence is not available as a signal here anyway: `loca.json`'s
        // permission arrays are `.default([])` in the schema, so "declared
        // nothing" and "declared an empty list" are the same value by the time
        // this code runs. A future editor reaching for a presence test would be
        // branching on a constant. See the note on `flag_set_to_args`.
        assert!(flag_set_to_args(&FlagSet::default()).is_empty());
        assert!(args_for(&[], &[]).is_empty());

        // The positive control: with something granted, flags DO appear. Both
        // assertions above hold for a function that returns nothing ever.
        assert_eq!(
            args_for(&[read("/tmp/x")], &[]),
            vec!["--allow-read=/tmp/x".to_string()]
        );
    }

    // ---- the self-write refusal -------------------------------------------

    #[test]
    fn a_write_grant_into_the_install_dir_is_refused() {
        // The install directory exactly, and a path under it. The exact case is
        // the one a `starts_with(root + separator)` port silently loses.
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
        // Normalisation happens BEFORE the comparison, so spelling the same
        // directory with a detour does not defeat the check.
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
        // The refusal is component-wise, not a textual prefix: `demo-2` shares a
        // prefix with `demo` and is a different extension's directory, so a
        // legitimate grant on it must survive. Without this, the refusal test
        // above passes just as well with `starts_with` on strings.
        let args = args_for(&[write("$extension-2/out.txt")], &[]);
        assert_eq!(
            args,
            vec!["--allow-write=/home/u/.loca/stable/core/extensions/demo-2/out.txt"]
        );
    }

    #[test]
    fn a_read_grant_into_the_install_dir_is_allowed() {
        // Only WRITE is refused. Reading its own code is what a worker does.
        let args = args_for(&[read("$extension/data.json")], &[]);
        assert_eq!(
            args,
            vec!["--allow-read=/home/u/.loca/stable/core/extensions/demo/data.json"]
        );
    }

    // ---- the empty expansion ----------------------------------------------

    #[test]
    fn an_empty_path_expansion_never_reaches_the_flag_list() {
        // `--allow-read=` is not "no grant": Deno reads the empty value as a far
        // broader grant than the author declared. An unsatisfiable path must
        // produce NO flag at all.
        let args = args_for(&[read("$env.TMPDIR"), write("$env.TMPDIR")], &[]);
        assert!(args.is_empty(), "expected no flags at all, got {args:?}");
        assert!(
            !args.iter().any(|a| a.ends_with('=')),
            "an empty-valued permission flag reached the args: {args:?}"
        );
    }

    #[test]
    fn a_resolvable_env_var_does_reach_the_flag_list() {
        // The positive control for the test above: without it, an expansion that
        // silently produced nothing in every case would still pass.
        let args = args_for(&[read("$env.TMPDIR/cache")], &[("TMPDIR", "/var/tmp")]);
        assert_eq!(args, vec!["--allow-read=/var/tmp/cache"]);
    }

    // ---- the $env allowlist -----------------------------------------------

    #[test]
    fn an_off_allowlist_env_var_resolves_to_nothing_and_takes_its_declaration_with_it() {
        // Two properties, and the second is the one a whole-result emptiness
        // check misses. The core process can hold an operator's API key, so
        // resolving it into a granted path would put its value in the process
        // argument list. And a declaration whose template resolved to NOTHING
        // grants nothing: `$env.OPENAI_API_KEY/x` must not become `/x`, which
        // is a read grant one component off the filesystem root that the author
        // never asked for.
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
        // The pattern is uppercase-and-underscore only, so `$env.home` is
        // literal text rather than an expansion.
        let args = args_for(&[read("/tmp/$env.home")], &[("home", "/home/u")]);
        assert_eq!(args, vec!["--allow-read=/tmp/$env.home"]);
    }

    // ---- the Deno syntax traps --------------------------------------------

    #[test]
    fn a_wildcard_host_emits_the_bare_allow_net_and_nothing_else() {
        // `--allow-net=*:80` is rejected by Deno at parse time, which kills the
        // worker at spawn with no useful diagnostic.
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

    // ---- what contributes and what does not -------------------------------

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
        // Two properties in one, and the second is what makes the first
        // testable. A stale row left over from a previous loca.json version is
        // inert, because the declaration is the vocabulary and a decision about
        // something no longer declared decides nothing. But a stale row can only
        // be OBSERVED to be inert if a loosened key comparison would let it
        // enable something: so `/tmp/ungranted` is declared with no grant of its
        // own, and a match on kind or on prefix rather than on the exact key
        // would hand it the sibling read grant.
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
        let decls = [
            PermissionDecl::Llm {
                optional: None,
                reason: "l".to_string(),
            },
            PermissionDecl::Tts {
                optional: None,
                reason: "t".to_string(),
            },
            PermissionDecl::Stt {
                optional: None,
                reason: "s".to_string(),
            },
            PermissionDecl::Memories {
                access: crate::contract::api::PermissionDeclAccess::Write,
                optional: None,
                reason: "m".to_string(),
            },
        ];
        assert!(args_for(&decls, &[]).is_empty());
    }

    #[test]
    fn every_flag_kind_renders_in_the_expected_order() {
        let decls = [
            read("/r"),
            write("/w"),
            net("h", &[port(1)]),
            PermissionDecl::Env {
                key: "K".to_string(),
                optional: None,
                reason: "e".to_string(),
            },
            run("b"),
            PermissionDecl::Ffi {
                optional: None,
                reason: "f".to_string(),
            },
            PermissionDecl::Sys {
                flag: "hostname".to_string(),
                optional: None,
                reason: "s".to_string(),
            },
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

    // ---- the permission key -----------------------------------------------

    #[test]
    fn permission_keys_match_the_persisted_strings() {
        // These strings are in the `grants` table on every existing install and
        // on the wire. A changed format silently orphans every grant.
        let cases: Vec<(PermissionDecl, &str)> = vec![
            (
                net("example.com", &[port(80), any_port()]),
                "net:example.com:80,*",
            ),
            (read("$home/x"), "read:$home/x"),
            (write("$home/x"), "write:$home/x"),
            (run("ffmpeg"), "run:ffmpeg"),
            (
                PermissionDecl::Env {
                    key: "PATH".to_string(),
                    optional: None,
                    reason: String::new(),
                },
                "env:PATH",
            ),
            (
                PermissionDecl::Ffi {
                    optional: None,
                    reason: String::new(),
                },
                "ffi",
            ),
            (
                PermissionDecl::Sys {
                    flag: "hostname".to_string(),
                    optional: None,
                    reason: String::new(),
                },
                "sys:hostname",
            ),
            (
                PermissionDecl::Memories {
                    access: crate::contract::api::PermissionDeclAccess::Read,
                    optional: None,
                    reason: String::new(),
                },
                "memories:read",
            ),
            (
                PermissionDecl::Llm {
                    optional: None,
                    reason: String::new(),
                },
                "llm",
            ),
            (
                PermissionDecl::Tts {
                    optional: None,
                    reason: String::new(),
                },
                "tts",
            ),
            (
                PermissionDecl::Stt {
                    optional: None,
                    reason: String::new(),
                },
                "stt",
            ),
        ];
        for (decl, want) in cases {
            assert_eq!(permission_key(&decl), want);
        }
    }

    // ---- the deny list ----------------------------------------------------

    #[test]
    fn the_deny_list_names_every_core_secret() {
        // Asserted by CONTENT, not by the presence of the flag: a test that
        // greps for `--deny-read` passes just as well with the list emptied.
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
        // `--deny-read` alone leaves the vault writable; `--deny-write` alone
        // leaves it readable. Deno's deny takes precedence over any allow, which
        // is what keeps a tool granted `$home` out of `~/.loca`.
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

    // ---- the containment primitive ----------------------------------------

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
