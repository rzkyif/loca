//! What a tool worker is started with: the argument vector and the environment.
//!
//! Three modes: PTY (unix, prompt context supplied) and Socket (Windows, same,
//! plus a loopback control channel because a ConPTY reflows stdout) keep Deno's
//! prompts; Pipe (no prompt context) adds `--no-prompt` so an ask-state
//! permission fails as `NotCapable` instead of blocking on a non-terminal.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::extensions::sandbox::{self, EnvLookup};
use crate::paths::Paths;

/// The non-secret operational environment a worker may need. Everything else
/// the core process holds (API keys, `LOCA_*`) is dropped; the keys the tool
/// was granted come back from its `--allow-env` list.
pub const WORKER_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    // Windows essentials.
    "SystemRoot",
    "SYSTEMROOT",
    "windir",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ComSpec",
    "PATHEXT",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A terminal, and the protocol on the worker's stdout.
    Pty,
    /// A pseudoconsole, and the protocol on a loopback socket.
    Socket,
    /// No terminal, and `--no-prompt`.
    Pipe,
}

impl Mode {
    /// Which mode a prompt context implies on this platform.
    pub fn for_platform(has_prompt_context: bool) -> Self {
        match (has_prompt_context, cfg!(windows)) {
            (false, _) => Self::Pipe,
            (true, false) => Self::Pty,
            (true, true) => Self::Socket,
        }
    }

    pub fn prompts(self) -> bool {
        !matches!(self, Self::Pipe)
    }
}

/// Where the control socket is, for a socket-mode spawn.
#[derive(Debug, Clone)]
pub struct ControlEndpoint {
    pub addr: String,
    pub token: String,
    pub port: u16,
}

/// Everything the plan needs that is not derived from `Paths`.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub extension_id: String,
    /// The module inside the extension the worker imports.
    pub entry_path: PathBuf,
    /// The extension's install directory. Passed to `--allow-read` and, when it
    /// has one, to `--config`.
    pub extension_folder: PathBuf,
    /// The derived `--allow-*` set for the INVOKED TOOL only, never the union
    /// of every enabled tool.
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnPlan {
    pub args: Vec<String>,
    pub env: BTreeMap<OsString, OsString>,
}

/// Build the argument vector and the environment.
pub fn plan(
    spec: &SpawnSpec,
    paths: &Paths,
    mode: Mode,
    control: Option<&ControlEndpoint>,
    env: &dyn EnvLookup,
) -> SpawnPlan {
    // Runtime-resolved: the worker's .ts ships separately from the binary.
    let worker_entry = paths.workers_dir().join("tool-worker.ts");
    // The extension's own runtime config; npm-only extensions have none.
    let config_path = spec.extension_folder.join("deno.json");
    let has_config = is_file(&config_path);
    // A deno.lock exists only once `deno install` ran; `--frozen` then stops the
    // worker rewriting a lockfile it holds no write grant for.
    let has_lock = is_file(&spec.extension_folder.join("deno.lock"));
    let denied = sandbox::core_secret_deny_paths(paths)
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(",");

    let mut flags = spec.flags.clone();
    if let Some(control) = control {
        merge_control_socket_grant(&mut flags, control.port);
    }

    let mut args = vec!["run".to_string()];
    if !mode.prompts() {
        args.push("--no-prompt".to_string());
    }
    args.push("--no-check".to_string());
    args.push("--quiet".to_string());
    args.push("--node-modules-dir=auto".to_string());
    if has_lock {
        args.push("--frozen".to_string());
    }
    args.extend(flags);
    // After the derived flags: Deno merges a repeated `--allow-read` (so this
    // can be appended) but rejects a repeated `--allow-net` (merged above).
    args.push(format!(
        "--allow-read={},{}",
        spec.extension_folder.display(),
        paths.deno_cache_dir().display()
    ));
    // Deny flags win over every allow, including a `$home` grant that contains
    // `~/.loca`; that is what keeps the vault out of reach.
    args.push(format!("--deny-read={denied}"));
    args.push(format!("--deny-write={denied}"));
    if has_config {
        args.push("--config".to_string());
        args.push(config_path.to_string_lossy().into_owned());
    }
    args.push(worker_entry.to_string_lossy().into_owned());
    args.push(format!("--extension-id={}", spec.extension_id));
    args.push(format!("--entry={}", spec.entry_path.display()));
    if let Some(control) = control {
        args.push(format!("--control-addr={}", control.addr));
        args.push(format!("--control-token={}", control.token));
    }

    SpawnPlan {
        args,
        env: plan_env(spec, paths, env),
    }
}

/// The spawn clears the inherited environment first, so this map is the whole
/// of it: `DENO_DIR`, the allowlist, and the keys the tool was granted AND the
/// core holds.
fn plan_env(spec: &SpawnSpec, paths: &Paths, env: &dyn EnvLookup) -> BTreeMap<OsString, OsString> {
    let mut out = BTreeMap::new();
    out.insert(
        OsString::from("DENO_DIR"),
        OsString::from(paths.deno_cache_dir()),
    );
    for key in WORKER_ENV_ALLOWLIST {
        if let Some(value) = env.get(key) {
            out.insert(OsString::from(key), OsString::from(value));
        }
    }
    // The tool's own flags, not the merged ones: the control socket grant is a
    // net grant and never adds an env key.
    for key in granted_env_keys(&spec.flags) {
        if let Some(value) = env.get(&key) {
            out.insert(OsString::from(&key), OsString::from(value));
        }
    }
    out
}

fn granted_env_keys(flags: &[String]) -> Vec<String> {
    flags
        .iter()
        .filter_map(|flag| flag.strip_prefix("--allow-env="))
        .flat_map(|list| list.split(','))
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .collect()
}

/// Deno rejects a repeated `--allow-net`, so the loopback port folds into the
/// tool's existing net flag; a bare `--allow-net` already covers it.
fn merge_control_socket_grant(flags: &mut Vec<String>, port: u16) {
    let grant = format!("127.0.0.1:{port}");
    match flags
        .iter()
        .position(|f| f == "--allow-net" || f.starts_with("--allow-net="))
    {
        None => flags.push(format!("--allow-net={grant}")),
        Some(i) if flags[i] == "--allow-net" => {}
        Some(i) => flags[i].push_str(&format!(",{grant}")),
    }
}

fn is_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file())
        .unwrap_or(false)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap as Map;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Map<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        paths: Paths,
        folder: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(tmp.path());
        let folder = paths.extensions_dir().join("demo");
        std::fs::create_dir_all(&folder).unwrap();
        Fixture {
            _tmp: tmp,
            paths,
            folder,
        }
    }

    fn spec(f: &Fixture, flags: &[&str]) -> SpawnSpec {
        SpawnSpec {
            extension_id: "demo".to_string(),
            entry_path: f.folder.join("index.ts"),
            extension_folder: f.folder.clone(),
            flags: flags.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn plan_of(f: &Fixture, flags: &[&str], mode: Mode) -> SpawnPlan {
        plan(&spec(f, flags), &f.paths, mode, None, &env(&[]))
    }

    fn socket_plan(f: &Fixture, flags: &[&str], port: u16) -> SpawnPlan {
        let control = ControlEndpoint {
            addr: format!("127.0.0.1:{port}"),
            token: "t".to_string(),
            port,
        };
        plan(
            &spec(f, flags),
            &f.paths,
            Mode::Socket,
            Some(&control),
            &env(&[]),
        )
    }

    fn net_flags(plan: &SpawnPlan) -> Vec<&String> {
        plan.args
            .iter()
            .filter(|a| a.starts_with("--allow-net"))
            .collect()
    }

    #[test]
    fn the_control_socket_grant_merges_into_an_existing_allow_net() {
        let f = fixture();
        let plan = socket_plan(
            &f,
            &["--allow-net=example.com:443", "--allow-read=/tmp"],
            5001,
        );
        let net = net_flags(&plan);
        assert_eq!(net.len(), 1, "exactly one --allow-net flag: {net:?}");
        assert_eq!(net[0], "--allow-net=example.com:443,127.0.0.1:5001");
    }

    #[test]
    fn the_control_socket_grant_is_added_when_the_tool_has_no_net_flag() {
        let f = fixture();
        let plan = socket_plan(&f, &[], 5002);
        assert!(plan
            .args
            .contains(&"--allow-net=127.0.0.1:5002".to_string()));
        assert!(plan
            .args
            .contains(&"--control-addr=127.0.0.1:5002".to_string()));
        assert!(plan.args.contains(&"--control-token=t".to_string()));
    }

    #[test]
    fn a_bare_allow_net_already_covers_the_control_socket_and_is_left_alone() {
        // `--allow-net=,127.0.0.1:5003` would NARROW a wildcard grant to the socket.
        let f = fixture();
        let plan = socket_plan(&f, &["--allow-net"], 5003);
        assert_eq!(net_flags(&plan), vec!["--allow-net"]);
    }

    #[test]
    fn both_deny_flags_are_present_and_follow_the_granted_read() {
        let f = fixture();
        let plan = plan_of(&f, &["--allow-read=/tmp"], Mode::Pty);
        let read_positions: Vec<usize> = plan
            .args
            .iter()
            .enumerate()
            .filter(|(_, a)| a.starts_with("--allow-read="))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            read_positions.len(),
            2,
            "the tool's read, then the folder's"
        );
        let deny_read = plan
            .args
            .iter()
            .position(|a| a.starts_with("--deny-read="))
            .expect("no --deny-read");
        let deny_write = plan
            .args
            .iter()
            .position(|a| a.starts_with("--deny-write="))
            .expect("no --deny-write");
        assert!(read_positions[1] < deny_read, "the deny flags come last");
        assert!(deny_read < deny_write);

        for path in sandbox::core_secret_deny_paths(&f.paths) {
            let path = path.to_string_lossy().into_owned();
            assert!(plan.args[deny_read].contains(&path), "missing {path}");
            assert!(plan.args[deny_write].contains(&path), "missing {path}");
        }
    }

    #[test]
    fn the_extension_folder_and_the_deno_cache_are_readable() {
        let f = fixture();
        let plan = plan_of(&f, &[], Mode::Pty);
        assert!(plan.args.contains(&format!(
            "--allow-read={},{}",
            f.folder.display(),
            f.paths.deno_cache_dir().display()
        )));
    }

    #[test]
    fn only_pipe_mode_disables_prompts() {
        let f = fixture();
        assert!(plan_of(&f, &[], Mode::Pipe)
            .args
            .contains(&"--no-prompt".to_string()));
        for mode in [Mode::Pty, Mode::Socket] {
            assert!(
                !plan_of(&f, &[], mode)
                    .args
                    .contains(&"--no-prompt".to_string()),
                "{mode:?} suppressed the prompts it exists to show"
            );
        }
    }

    #[test]
    fn a_prompt_context_selects_a_prompting_mode_on_every_desktop_platform() {
        assert_eq!(Mode::for_platform(false), Mode::Pipe);
        assert!(Mode::for_platform(true).prompts());
        assert_eq!(
            Mode::for_platform(true),
            if cfg!(windows) {
                Mode::Socket
            } else {
                Mode::Pty
            }
        );
    }

    #[test]
    fn frozen_and_config_appear_only_when_their_files_do() {
        let f = fixture();
        let bare = plan_of(&f, &[], Mode::Pty);
        assert!(!bare.args.contains(&"--frozen".to_string()));
        assert!(!bare.args.contains(&"--config".to_string()));

        std::fs::write(f.folder.join("deno.lock"), "{}").unwrap();
        std::fs::write(f.folder.join("deno.json"), "{}").unwrap();
        let full = plan_of(&f, &[], Mode::Pty);
        assert!(full.args.contains(&"--frozen".to_string()));
        let config = full
            .args
            .iter()
            .position(|a| a == "--config")
            .expect("no --config");
        assert_eq!(
            full.args[config + 1],
            f.folder.join("deno.json").to_string_lossy()
        );
    }

    #[test]
    fn the_worker_entry_and_its_arguments_come_after_every_flag() {
        let f = fixture();
        let plan = plan_of(&f, &["--allow-ffi"], Mode::Pty);
        let entry = f
            .paths
            .workers_dir()
            .join("tool-worker.ts")
            .to_string_lossy()
            .into_owned();
        let at = plan
            .args
            .iter()
            .position(|a| *a == entry)
            .expect("no entry");
        assert!(
            plan.args[..at]
                .iter()
                .all(|a| a.starts_with('-') || a == "run"),
            "a positional argument reached Deno before the script: {:?}",
            plan.args
        );
        assert_eq!(plan.args[at + 1], "--extension-id=demo");
        assert_eq!(
            plan.args[at + 2],
            format!("--entry={}", f.folder.join("index.ts").display())
        );
    }

    #[test]
    fn the_worker_environment_is_the_allowlist_plus_exactly_the_granted_keys() {
        let f = fixture();
        let ambient = env(&[
            ("PATH", "/usr/bin"),
            ("HOME", "/home/u"),
            ("OPENAI_API_KEY", "sk-secret"),
            ("LOCA_CHANNEL", "stable"),
            ("GRANTED_KEY", "visible"),
            ("USER", "someone"),
        ]);
        let plan = plan(
            &spec(&f, &["--allow-env=GRANTED_KEY"]),
            &f.paths,
            Mode::Pty,
            None,
            &ambient,
        );
        let got: Vec<String> = plan
            .env
            .keys()
            .map(|k| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            got,
            vec![
                "DENO_DIR".to_string(),
                "GRANTED_KEY".to_string(),
                "HOME".to_string(),
                "PATH".to_string(),
            ],
            "unexpected worker environment",
        );
        assert_eq!(
            plan.env.get(&OsString::from("DENO_DIR")),
            Some(&OsString::from(f.paths.deno_cache_dir()))
        );
    }

    #[test]
    fn an_ungranted_key_is_absent_even_when_the_core_holds_it() {
        let f = fixture();
        let ambient = env(&[("OPENAI_API_KEY", "sk-secret"), ("PATH", "/usr/bin")]);
        let plan = plan(&spec(&f, &[]), &f.paths, Mode::Pty, None, &ambient);
        assert!(!plan.env.contains_key(&OsString::from("OPENAI_API_KEY")));
        assert!(
            !plan
                .env
                .values()
                .any(|v| v.to_string_lossy().contains("sk-secret")),
            "a secret value reached the worker environment"
        );
        assert!(plan.env.contains_key(&OsString::from("PATH")));
    }

    #[test]
    fn a_granted_key_the_core_does_not_hold_is_simply_absent() {
        let f = fixture();
        let plan = plan(
            &spec(&f, &["--allow-env=NOT_SET,ALSO_NOT_SET"]),
            &f.paths,
            Mode::Pty,
            None,
            &env(&[]),
        );
        assert_eq!(
            plan.env.keys().collect::<Vec<_>>(),
            vec![&OsString::from("DENO_DIR")]
        );
    }

    #[test]
    fn an_empty_entry_in_the_allow_env_list_is_skipped() {
        // `--allow-env=A,,B` is what a trailing comma produces; an empty key is
        // an error on Windows rather than a miss.
        assert_eq!(
            granted_env_keys(&["--allow-env=A,,B".to_string()]),
            vec!["A".to_string(), "B".to_string()]
        );
        assert!(granted_env_keys(&["--allow-read=/tmp".to_string()]).is_empty());
    }
}
