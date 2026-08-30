//! The stdio transport: a local command, spawned, spoken to over its pipes.
//!
//! Every subprocess line in this package is here, so the half that mobile
//! cannot run is one file rather than a thread through the manager. The gate is
//! [`StdioSupport`], a value the frontend supplies rather than a `cfg`: a `cfg`
//! would leave the desktop suite as the only thing that ever compiles this, and
//! the mobile gap would be invisible on the machine where the code is written.
//!
//! The child is spawned through `sidecars::Manager::spawn`, which gives it Job
//! Object membership on Windows, an environment allowlist, a graceful
//! SIGTERM-then-SIGKILL terminate and a kill-on-drop backstop. rmcp's own
//! `transport-child-process` feature would be a second spawn path with none of
//! that, which is why it is not enabled.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rmcp::ServiceExt;
use tokio::io::AsyncReadExt;

use crate::error::{internal, AppError, ErrorCode, Result};
use crate::paths::Paths;
use crate::secrets;
use crate::sidecars::process::{inherited, OPERATIONAL_ENV_KEYS};
use crate::sidecars::{ChildHandle, Manager as Sidecars, Output, Spawn, Stdin};

use super::registry::ServerConfig;
use super::session::{self, Connected, Handshake, HANDSHAKE_TIMEOUT};
use crate::contract::api::McpStdioRuntime;

/// How much of a failing child's stderr is worth showing the user.
const STDERR_CAP: usize = 4_000;

/// Whether this device can run a local subprocess at all.
///
/// Mobile cannot: Android's W^X and iOS both forbid it. An stdio server stays
/// configurable there, so a phone paired to a desktop core can still edit one,
/// but connecting is refused before anything is spawned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdioSupport {
    Available,
    Unsupported,
}

impl StdioSupport {
    pub fn is_available(self) -> bool {
        self == Self::Available
    }
}

/// Everything needed to start one stdio server, resolved but not yet spawned.
///
/// Separate from the spawn so it can be asserted in a test without a child
/// process: the permission flags, the deny list and the environment are the
/// parts worth pinning, and they are pure functions of the config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnPlan {
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<OsString, OsString>,
}

/// Resolve what to run, or say why it cannot be run.
pub fn plan(server: &ServerConfig, paths: &Paths, support: StdioSupport) -> Result<SpawnPlan> {
    if !support.is_available() {
        return Err(AppError::new(
            ErrorCode::ServerUnavailable,
            "This device cannot run a local MCP server. Use a remote server instead.",
        ));
    }
    let label = format!("mcp server {}", server.name);
    match server.runtime {
        McpStdioRuntime::Deno => plan_deno(server, paths, label),
        McpStdioRuntime::Custom => {
            let command = server
                .command
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .ok_or_else(|| {
                    AppError::new(
                        ErrorCode::ValidationError,
                        "stdio MCP server has no command",
                    )
                })?;
            let env = inherited(OPERATIONAL_ENV_KEYS);
            // The same lookup the spawn would do, reported here rather than
            // after a failed spawn so the message is actionable.
            if !resolves(command, env.get(&OsString::from("PATH"))) {
                return Err(AppError::new(
                    ErrorCode::ServerUnavailable,
                    launcher_unavailable_message(command),
                ));
            }
            Ok(SpawnPlan {
                label,
                program: PathBuf::from(command),
                args: server.args.clone(),
                env,
            })
        }
    }
}

/// The bundled deno runtime: `command` is a run target (an `npm:` specifier, a
/// URL, a script), so an npm-based server needs no Node.js install.
fn plan_deno(server: &ServerConfig, paths: &Paths, label: String) -> Result<SpawnPlan> {
    let target = server
        .command
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::ValidationError,
                "deno MCP server has no package or script to run",
            )
        })?;
    let deno = crate::sidecars::deno::resolve_worker_deno(paths)?;
    let mut args = vec!["run".to_string()];
    if server.deno_allow_all {
        // Even under allow-all, the grant is backstopped with the extension
        // workers' secret-file deny list. Deno's `--deny-*` wins over
        // `--allow-*`, so a deno MCP server still cannot reach the core's
        // secrets, admin token, database, memories or another extension's data.
        let deny = deny_list(paths);
        args.push("--allow-all".to_string());
        args.push(format!("--deny-read={deny}"));
        args.push(format!("--deny-write={deny}"));
    } else {
        // Least privilege: the user's manual flags and nothing else, which
        // makes the deny list redundant but harmless.
        args.extend(server.deno_permissions.iter().cloned());
    }
    args.push(target.to_string());
    args.extend(server.args.iter().cloned());

    let mut env = inherited(OPERATIONAL_ENV_KEYS);
    // Contain the npm/deno cache under Loca's channel directory rather than the
    // user's global one.
    env.insert(
        OsString::from("DENO_DIR"),
        OsString::from(paths.deno_cache_dir()),
    );
    Ok(SpawnPlan {
        label,
        program: deno,
        args,
        env,
    })
}

/// The core's private paths, which a sandboxed child must never read or write
/// even under a broad grant.
///
/// Includes the `.master-key` transient the atomic master-key write creates.
fn deny_list(paths: &Paths) -> String {
    let root = paths.root();
    let db = paths.db_file();
    let entries: Vec<PathBuf> = vec![
        paths.secrets_enc_file(),
        with_suffix(&paths.secrets_enc_file(), ".tmp"),
        paths.secrets_plain_file(),
        secrets::master_key_file(root),
        secrets::master_key_tmp_file(root),
        paths.admin_token_file(),
        paths.admin_password_file(),
        db.clone(),
        with_suffix(&db, "-wal"),
        with_suffix(&db, "-shm"),
        with_suffix(&db, "-journal"),
        root.join("extension-data"),
        paths.memories_dir(),
    ];
    entries
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(",")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Whether the program would be found, using the same rule the spawn does: an
/// explicit path is checked directly, a bare name is searched on `PATH`.
fn resolves(command: &str, path_var: Option<&OsString>) -> bool {
    let as_path = Path::new(command);
    if as_path.components().count() > 1 || as_path.is_absolute() {
        return as_path.is_file();
    }
    let Some(path_var) = path_var else {
        return false;
    };
    std::env::split_paths(path_var).any(|dir| {
        let candidate = dir.join(command);
        candidate.is_file() || windows_variants(&candidate).any(|c| c.is_file())
    })
}

/// On Windows a bare `npx` is `npx.cmd` on disk. Elsewhere this yields nothing.
///
/// The extensions come from the user's `PATHEXT` when set, because that is the
/// list the OS itself searches; a hard-coded list would turn a launcher outside
/// it into a confident "was not found" for a command that would have run.
fn windows_variants(candidate: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    const DEFAULT_EXTENSIONS: &str = ".EXE;.CMD;.BAT;.COM";
    if !cfg!(windows) {
        return Vec::new().into_iter();
    }
    let configured = std::env::var("PATHEXT")
        .ok()
        .filter(|v| !v.trim().is_empty());
    configured
        .unwrap_or_else(|| DEFAULT_EXTENSIONS.to_string())
        .split(';')
        .map(str::trim)
        .filter(|ext| !ext.is_empty())
        .map(|ext| with_suffix(candidate, ext))
        .collect::<Vec<_>>()
        .into_iter()
}

/// Launchers that ship with Node.js or a Python tool, so a "not found" can
/// point somewhere concrete instead of at a bare "command not found".
const NODE_LAUNCHERS: [&str; 7] = ["npx", "npm", "node", "bunx", "bun", "yarn", "pnpm"];
const PYTHON_LAUNCHERS: [&str; 5] = ["uvx", "uv", "pipx", "python", "python3"];

fn launcher_unavailable_message(command: &str) -> String {
    let name = launcher_basename(command);
    if NODE_LAUNCHERS.contains(&name.as_str()) {
        return format!(
            "\"{name}\" was not found. Install Node.js, or switch Runtime to \
             Bundled deno and set Package or Script to an npm specifier (for example \
             npm:@scope/server), which runs many npm-based MCP servers without Node.js."
        );
    }
    if PYTHON_LAUNCHERS.contains(&name.as_str()) {
        return format!("\"{name}\" was not found. Install it to run this MCP server.");
    }
    format!("\"{name}\" was not found on this machine. Check the command or install it.")
}

fn launcher_basename(command: &str) -> String {
    let trimmed = command.trim();
    let base = trimmed
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(trimmed)
        .to_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stem) = base.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    base
}

/// Spawn the planned command and speak MCP over its pipes.
pub async fn connect(
    server: &ServerConfig,
    paths: &Paths,
    sidecars: &Sidecars,
    support: StdioSupport,
) -> Result<Connected> {
    let plan = plan(server, paths, support)?;
    let spec = Spawn::new(plan.label.clone(), plan.program.clone())
        .args(&plan.args)
        .envs(plan.env.clone())
        .stdin(Stdin::Piped)
        .stdout(Output::Piped)
        // Drained here rather than by the sidecar relay: the connect-failure
        // status is user-visible and needs the child's own words.
        .stderr(Output::Piped);
    let child: ChildHandle = sidecars.spawn(spec)?;

    let (stdout, stdin, stderr) = {
        let mut guard = child.lock().await;
        (guard.take_stdout(), guard.take_stdin(), guard.take_stderr())
    };
    let (Some(stdout), Some(stdin)) = (stdout, stdin) else {
        return Err(internal("MCP stdio child has no pipes"));
    };
    let tail = stderr.map(drain_stderr);

    let (handler, events) = session::event_channel();
    let tx = session::sender(&handler);
    // Bounded for the same reason the remote transport is: a child that starts
    // cleanly and then never writes to its stdout would otherwise hold this
    // connect open, and the child with it, for the life of the process.
    let served = session::handshake(HANDSHAKE_TIMEOUT, handler.serve((stdout, stdin))).await;
    let service = match served {
        Handshake::Ready(service) => service,
        Handshake::Failed(err) => {
            // Fold in whatever the child printed, which is where a deno
            // PermissionDenied from a too-narrow manual permission set or a
            // missing module actually says so.
            terminate(child);
            return Err(AppError::new(
                ErrorCode::ServerUnavailable,
                with_stderr(format!("{err}"), tail.as_ref()),
            ));
        }
        Handshake::TimedOut => {
            terminate(child);
            return Err(AppError::new(
                ErrorCode::ServerUnavailable,
                with_stderr(
                    session::handshake_timeout_message(HANDSHAKE_TIMEOUT),
                    tail.as_ref(),
                ),
            ));
        }
    };
    let session = session::adopt(
        service,
        tx,
        // The closure owns the child handle, so the process lives exactly as long
        // as the session. Dropping the last handle would kill it through the
        // sidecar backstop instead, a harsher exit for the same outcome.
        Some(Box::new(move || terminate(child))),
    );
    Ok(Connected { session, events })
}

/// Append whatever the child printed, when it printed anything.
fn with_stderr(message: String, tail: Option<&Tail>) -> String {
    match tail.map(read_tail) {
        Some(tail) if !tail.is_empty() => format!("{message}: {tail}"),
        _ => message,
    }
}

/// Stop the child, gracefully, without making the caller async.
fn terminate(child: ChildHandle) {
    tokio::spawn(async move {
        child.lock().await.terminate().await;
    });
}

type Tail = Arc<Mutex<String>>;

/// Keep reading the child's stderr for as long as it runs, retaining the first
/// [`STDERR_CAP`] characters.
///
/// Reading to EOF is not optional: an undrained pipe fills at about 64 KB and
/// then blocks the child's next write forever, so a chatty server would hang
/// rather than fail.
fn drain_stderr(mut stderr: tokio::process::ChildStderr) -> Tail {
    let tail: Tail = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&tail);
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut held = sink.lock().unwrap_or_else(|e| e.into_inner());
                    if held.len() < STDERR_CAP {
                        held.push_str(&String::from_utf8_lossy(&buf[..n]));
                        held.truncate(STDERR_CAP);
                    }
                }
            }
        }
    });
    tail
}

fn read_tail(tail: &Tail) -> String {
    tail.lock()
        .unwrap_or_else(|e| e.into_inner())
        .trim()
        .to_string()
}
