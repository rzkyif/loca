//! Where a (extension, tool) pair becomes a running process.
//!
//! The flags come from the INVOKED TOOL's grants alone, never the union over
//! the extension's tools; the pool's per-(extension, tool) key is what makes
//! that expressible. Only the pipe launch goes through `sidecars::Manager::spawn`;
//! the PTY and socket launches need fds and a `pre_exec` hook `Spawn` cannot
//! express, so they spawn their own child and enrol it for the Windows Job
//! Object backstop afterwards.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::AsyncReadExt as _;
use tokio::sync::mpsc;

use crate::error::{internal, Result};
use crate::extensions::prompt::matcher::PromptPolicy;
use crate::extensions::registry::Registry;
use crate::extensions::sandbox::{self, EnvLookup, PathTemplates, ProcessEnv, ToolGrants};
use crate::extensions::socket::ControlListener;
use crate::paths::Paths;
use crate::sidecars::deno::resolve_worker_deno;
use crate::sidecars::{ChildHandle, Manager, Output, Spawn, Stdin};

use super::entry::resolve_entry_path;
use super::handle::{pump_protocol, Incoming, Terminal, WorkerHandle, MAX_PROTOCOL_FRAME_BYTES};
use super::pool::{WorkerKey, WorkerSpawner};
use super::spawn_plan::{self, ControlEndpoint, Mode, SpawnPlan, SpawnSpec};

/// The production [`WorkerSpawner`]: the pinned `deno`, the registry's facts
/// about one tool, and a terminal.
pub struct DenoSpawner {
    paths: Paths,
    registry: Registry,
    sidecars: Arc<Manager>,
    env: Arc<dyn EnvLookup + Send + Sync>,
}

/// What a spawn resolved to, before any process exists.
#[derive(Debug)]
struct Recipe {
    spec: SpawnSpec,
    mode: Mode,
    /// `None` when the tool is not in the registry for this host, which is also
    /// what puts the worker in pipe mode: a prompt that cannot be decided
    /// against declarations must not be shown.
    policy: Option<PromptPolicy>,
}

/// A started worker's two ends: what Core writes to, and what it reads from.
struct Launched {
    terminal: Arc<dyn Terminal>,
    incoming: mpsc::UnboundedReceiver<Incoming>,
}

impl DenoSpawner {
    pub fn new(paths: Paths, registry: Registry, sidecars: Arc<Manager>) -> Self {
        Self {
            paths,
            registry,
            sidecars,
            env: Arc::new(ProcessEnv),
        }
    }

    /// The same spawner over a fixed environment; `std::env::set_var` is
    /// unsound in a threaded test binary, so injecting is the only way to test.
    pub fn with_env(
        paths: Paths,
        registry: Registry,
        sidecars: Arc<Manager>,
        env: Arc<dyn EnvLookup + Send + Sync>,
    ) -> Self {
        Self {
            paths,
            registry,
            sidecars,
            env,
        }
    }

    async fn recipe(&self, key: &WorkerKey) -> Result<Recipe> {
        let extension = self.registry.get_or_err(&key.extension_id).await?;
        let folder = PathBuf::from(&extension.installed_path);
        // Through `list_tools`, so a tool this OS does not support is ABSENT
        // (pipe mode, no grants) rather than present with an empty set, which
        // would offer an `ask` prompt for a tool that cannot run here.
        let tool = self
            .registry
            .list_tools(&key.extension_id)
            .await?
            .into_iter()
            .find(|tool| tool.name == key.tool_name);

        let templates =
            PathTemplates::for_extension(&self.paths, folder.clone(), self.env.as_ref());
        let grants: Vec<ToolGrants> = tool
            .iter()
            .map(|tool| ToolGrants {
                required: tool.required_permissions.clone(),
                grants: tool.grants.clone(),
            })
            .collect();
        let flag_set = sandbox::union_flags(&grants, &templates, self.env.as_ref());

        let elevated = flag_set.elevated(&templates.home);
        if !elevated.is_empty() {
            tracing::warn!(
                target: "loca::extensions",
                "tool {}/{} granted elevated permissions: {}",
                key.extension_id,
                key.tool_name,
                elevated.join(", "),
            );
        }

        Ok(Recipe {
            mode: Mode::for_platform(tool.is_some()),
            policy: tool.map(|tool| PromptPolicy {
                required: tool.required_permissions,
                grants: tool.grants,
                undeclared_policy: extension.undeclared_policy,
                templates,
            }),
            spec: SpawnSpec {
                extension_id: key.extension_id.clone(),
                entry_path: resolve_entry_path(&folder),
                extension_folder: folder,
                flags: sandbox::flag_set_to_args(&flag_set),
            },
        })
    }

    async fn launch(
        &self,
        program: &Path,
        plan: SpawnPlan,
        mode: Mode,
        listener: Option<ControlListener>,
        label: &str,
        extension_id: &str,
    ) -> Result<Launched> {
        match mode {
            Mode::Pipe => self.launch_piped(program, plan, label, extension_id).await,
            Mode::Pty | Mode::Socket => {
                self.launch_terminal(program, plan, listener, label, extension_id)
            }
        }
    }

    /// No terminal: three pipes, through the manager so the child joins the
    /// Windows Job Object.
    async fn launch_piped(
        &self,
        program: &Path,
        plan: SpawnPlan,
        label: &str,
        extension_id: &str,
    ) -> Result<Launched> {
        let child: ChildHandle = self.sidecars.spawn(
            Spawn::new(label, program)
                .args(&plan.args)
                .envs(plan.env)
                .stdin(Stdin::Piped)
                .stdout(Output::Piped)
                // Piped, not relayed: stderr carries the tool's own log lines,
                // which belong to the call.
                .stderr(Output::Piped),
        )?;

        let (stdin, stdout, stderr) = {
            let mut guard = child.lock().await;
            (guard.take_stdin(), guard.take_stdout(), guard.take_stderr())
        };
        let (Some(mut stdin), Some(stdout)) = (stdin, stdout) else {
            return Err(internal(format!("{label}: the worker has no pipes")));
        };

        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (kill_tx, mut kill_rx) = mpsc::unbounded_channel::<()>();

        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt as _;
            while let Some(bytes) = write_rx.recv().await {
                if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        if let Some(stderr) = stderr {
            spawn_terminal_pump(stderr, incoming_tx.clone());
        }

        // One task owns the child handle and is the only thing that locks it: a
        // reaper holding the lock across `wait` would block the kill meant to
        // make that wait return.
        let pump = pump_protocol(stdout, extension_id.to_string(), incoming_tx.clone());
        tokio::spawn(async move {
            tokio::select! {
                () = pump => {}
                _ = kill_rx.recv() => {}
            }
            // Idempotent and reaping: an exited child is collected, a wedged
            // one gets SIGTERM then SIGKILL.
            let exit = child.lock().await.terminate().await;
            let _ = incoming_tx.send(Incoming::Exited { code: exit.code });
        });

        Ok(Launched {
            terminal: Arc::new(PipeChannel {
                writes: write_tx,
                kill: kill_tx,
            }),
            incoming: incoming_rx,
        })
    }

    /// Under a terminal, so Deno prompts.
    fn launch_terminal(
        &self,
        program: &Path,
        plan: SpawnPlan,
        listener: Option<ControlListener>,
        label: &str,
        extension_id: &str,
    ) -> Result<Launched> {
        use crate::extensions::pty;

        let spawned = pty::spawn(pty::PtySpawn {
            label: label.to_string(),
            program: program.to_path_buf(),
            args: plan.args,
            env: plan.env,
            // The worker resolves everything by absolute path.
            cwd: None,
        })?;
        // A PTY spawn cannot go through `Manager::spawn`; without this a hard
        // core kill on Windows leaves the worker running.
        if let Some(pid) = spawned.pid {
            self.sidecars.track_external(pid, label);
        }

        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let session = spawned.session;
        let mut events = spawned.events;
        let event_tx = incoming_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let forwarded = match event {
                    pty::PtyEvent::Output(bytes) => event_tx.send(Incoming::Terminal(bytes)),
                    pty::PtyEvent::Exited { code } => event_tx.send(Incoming::Exited { code }),
                };
                if forwarded.is_err() {
                    break;
                }
            }
        });

        match (spawned.stdout, listener) {
            // unix: the protocol rides the ordinary stdout pipe.
            (Some(stdout), _) => {
                tokio::spawn(pump_protocol(stdout, extension_id.to_string(), incoming_tx));
                Ok(Launched {
                    terminal: Arc::new(session),
                    incoming: incoming_rx,
                })
            }
            // Windows: the pseudoconsole reflows stdout, so the protocol rides
            // the loopback socket and the console carries only the prompt.
            (None, Some(listener)) => {
                let (frame_tx, frame_rx) = mpsc::unbounded_channel::<String>();
                spawn_control_pump(listener, frame_rx, incoming_tx, session.clone(), label);
                Ok(Launched {
                    terminal: Arc::new(SocketChannel {
                        frames: frame_tx,
                        console: session,
                    }),
                    incoming: incoming_rx,
                })
            }
            (None, None) => Err(internal(format!(
                "{label}: a terminal worker with neither a protocol pipe nor a control socket"
            ))),
        }
    }
}

#[async_trait]
impl WorkerSpawner for DenoSpawner {
    async fn spawn(&self, key: &WorkerKey) -> Result<WorkerHandle> {
        // First, so a missing runtime reports `binary_not_found` (which the
        // client has a path for) rather than a bare spawn failure.
        let program = resolve_worker_deno(&self.paths)?;
        let recipe = self.recipe(key).await?;

        // Bound BEFORE the args are built: the port is granted via `--allow-net`.
        let listener = match recipe.mode {
            Mode::Socket => Some(ControlListener::bind().await?),
            Mode::Pty | Mode::Pipe => None,
        };
        let endpoint = listener.as_ref().map(|listener| ControlEndpoint {
            addr: listener.addr().to_string(),
            token: listener.token().to_string(),
            port: listener.port(),
        });
        let plan = spawn_plan::plan(
            &recipe.spec,
            &self.paths,
            recipe.mode,
            endpoint.as_ref(),
            self.env.as_ref(),
        );

        let label = format!("tool worker {}/{}", key.extension_id, key.tool_name);
        let launched = self
            .launch(
                &program,
                plan,
                recipe.mode,
                listener,
                &label,
                &key.extension_id,
            )
            .await?;

        Ok(WorkerHandle::start(
            key.extension_id.clone(),
            launched.incoming,
            Some(launched.terminal),
            recipe.policy.map(Arc::new),
            Arc::clone(&self.env),
        ))
    }
}

/// A worker with no terminal. [`Terminal`] is the worker's whole write side,
/// so pipe mode needs one for call frames; it has no answer path because
/// `--no-prompt` means nothing will ever ask.
struct PipeChannel {
    writes: mpsc::UnboundedSender<Vec<u8>>,
    kill: mpsc::UnboundedSender<()>,
}

impl Terminal for PipeChannel {
    fn write(&self, bytes: &[u8]) {
        let _ = self.writes.send(bytes.to_vec());
    }

    fn answer(&self, _bytes: &[u8]) {
        // Deliberately nothing: a `y`/`n` on stdin would be an unparseable
        // line in the protocol stream.
    }

    fn kill(&self) {
        let _ = self.kill.send(());
    }
}

/// A worker whose protocol rides the control socket while its prompt rides a
/// pseudoconsole. Windows only.
struct SocketChannel {
    frames: mpsc::UnboundedSender<String>,
    console: crate::extensions::pty::PtySession,
}

impl Terminal for SocketChannel {
    fn write(&self, bytes: &[u8]) {
        // Queued: the worker connects back some milliseconds after it starts,
        // and a frame written before that would be dropped.
        let _ = self
            .frames
            .send(String::from_utf8_lossy(bytes).into_owned());
    }

    fn answer(&self, bytes: &[u8]) {
        self.console.answer(bytes);
    }

    fn kill(&self) {
        self.console.kill();
    }
}

/// Terminal bytes (a worker's stderr, and Deno's prompt) into the driver.
fn spawn_terminal_pump<R>(mut reader: R, tx: mpsc::UnboundedSender<Incoming>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(Incoming::Terminal(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// Accept the worker's control connection, then pump it both ways. Two tasks:
/// `read_line` and `write_line` each need `&mut`, and serialising them would
/// make an outbound frame wait for the worker's next line.
fn spawn_control_pump(
    listener: ControlListener,
    mut outbound: mpsc::UnboundedReceiver<String>,
    inbound: mpsc::UnboundedSender<Incoming>,
    console: crate::extensions::pty::PtySession,
    label: &str,
) {
    let label = label.to_string();
    tokio::spawn(async move {
        let channel = match listener.accept().await {
            Ok(channel) => channel,
            Err(error) => {
                // Fail closed: a worker that never connects can never be
                // spoken to, so kill it rather than wait for the boot timeout.
                tracing::warn!(
                    target: "loca::extensions",
                    "{label}: the worker never connected to its control socket: {error}",
                );
                console.kill();
                return;
            }
        };
        let (mut reader, mut writer) = channel.split();
        tokio::spawn(async move {
            while let Some(line) = outbound.recv().await {
                if writer.write_line(&line).await.is_err() {
                    break;
                }
            }
            writer.shutdown().await;
        });
        while let Some(line) = reader.read_line(MAX_PROTOCOL_FRAME_BYTES).await {
            if inbound.send(Incoming::ProtocolLine(line)).is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use crate::contract::api::{
        ExtensionSource, ExtensionStatus, GrantState, PermissionDecl, UndeclaredPolicy,
    };
    use crate::db::Db;
    use crate::extensions::registry::{ExtensionInsert, ToolInsert};
    use crate::extensions::sandbox::{permission_key, permission_kind_of};

    use super::*;

    /// Every test here can hang rather than fail, so every await is bounded.
    const BUDGET: Duration = Duration::from_secs(20);

    struct Fixture {
        _home: tempfile::TempDir,
        paths: Paths,
        registry: Registry,
        spawner: DenoSpawner,
    }

    fn fixture(env: BTreeMap<String, String>) -> Fixture {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(home.path());
        paths.ensure_dirs().unwrap();
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| crate::db::migrate(conn)).unwrap();
        let registry = Registry::with_platforms(
            db,
            paths.clone(),
            vec![crate::extensions::registry::host_platforms()
                .first()
                .cloned()
                .unwrap_or_default()],
        );
        let spawner = DenoSpawner::with_env(
            paths.clone(),
            registry.clone(),
            Arc::new(Manager::new()),
            Arc::new(env),
        );
        Fixture {
            _home: home,
            paths,
            registry,
            spawner,
        }
    }

    /// An extension on disk with one tool.
    async fn install(fx: &Fixture, id: &str, tool: ToolInsert, entry: &str) {
        let folder = fx.paths.extension_dir(id);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("index.ts"), entry).unwrap();
        fx.registry
            .upsert_extension(ExtensionInsert {
                id: id.to_string(),
                source: ExtensionSource::Local,
                display_name: id.to_string(),
                description: None,
                version: "1.0.0".to_string(),
                installed_path: folder.to_string_lossy().into_owned(),
                manifest_hash: "mh".to_string(),
                content_hash: "ch".to_string(),
                status: ExtensionStatus::Installed,
                has_deps: false,
                has_database: false,
            })
            .await
            .unwrap();
        fx.registry.replace_tools(id, vec![tool]).await.unwrap();
    }

    fn tool(name: &str, permissions: Vec<PermissionDecl>) -> ToolInsert {
        ToolInsert {
            name: name.to_string(),
            description: format!("does {name}"),
            parameters: serde_json::Map::new(),
            triggers: Vec::new(),
            fn_export: "run".to_string(),
            always_available: false,
            platforms: Vec::new(),
            required_permissions: permissions,
        }
    }

    fn read_decl(path: &str) -> PermissionDecl {
        PermissionDecl::Read {
            optional: None,
            path: path.to_string(),
            reason: "to read".to_string(),
        }
    }

    async fn grant(fx: &Fixture, tool_id: &str, decl: &PermissionDecl) {
        fx.registry
            .set_grants(
                tool_id,
                vec![crate::extensions::registry::GrantEntry {
                    key: permission_key(decl),
                    kind: permission_kind_of(decl),
                    state: GrantState::Granted,
                }],
            )
            .await
            .unwrap();
    }

    async fn recipe(fx: &Fixture, tool: &str) -> Recipe {
        fx.spawner
            .recipe(&WorkerKey::new("acme", tool))
            .await
            .unwrap()
    }

    /// A stand-in for the pinned `deno`: a real process without the download.
    /// It ignores its argument vector; `spawn_plan` owns the flags.
    fn plant_deno(paths: &Paths, script: &str) {
        let path = paths.bin_path(crate::sidecars::deno::binary_name());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[cfg(unix)]
    async fn spawn_acme(fx: &Fixture) -> WorkerHandle {
        tokio::time::timeout(BUDGET, fx.spawner.spawn(&WorkerKey::new("acme", "t")))
            .await
            .expect("the spawn hung")
            .expect("the spawn failed")
    }

    #[cfg(unix)]
    async fn launch_piped(fx: &Fixture) -> Launched {
        tokio::time::timeout(
            BUDGET,
            fx.spawner.launch(
                &fx.paths.bin_path(crate::sidecars::deno::binary_name()),
                SpawnPlan {
                    args: Vec::new(),
                    env: BTreeMap::new(),
                },
                Mode::Pipe,
                None,
                "test worker",
                "acme",
            ),
        )
        .await
        .expect("the launch hung")
        .expect("the launch failed")
    }

    #[tokio::test]
    async fn a_missing_runtime_is_reported_as_the_download_it_is() {
        let fx = fixture(BTreeMap::new());
        let err = fx
            .spawner
            .spawn(&WorkerKey::new("nothing", "at-all"))
            .await
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::BinaryNotFound);
    }

    #[tokio::test]
    async fn the_flags_come_from_the_invoked_tool_alone() {
        let fx = fixture(BTreeMap::new());
        let folder = fx.paths.extension_dir("acme");
        let noisy = read_decl("/tmp/loud");
        let quiet = read_decl("/tmp/soft");
        install(&fx, "acme", tool("noisy", vec![noisy.clone()]), "export {}").await;
        fx.registry
            .replace_tools(
                "acme",
                vec![
                    tool("noisy", vec![noisy.clone()]),
                    tool("quiet", vec![quiet.clone()]),
                ],
            )
            .await
            .unwrap();
        grant(&fx, "acme::noisy", &noisy).await;
        grant(&fx, "acme::quiet", &quiet).await;

        let noisy = recipe(&fx, "noisy").await;
        assert_eq!(noisy.spec.flags, vec!["--allow-read=/tmp/loud".to_string()]);
        assert_eq!(noisy.spec.extension_folder, folder);
        assert_eq!(noisy.spec.entry_path, folder.join("index.ts"));

        let quiet = recipe(&fx, "quiet").await;
        assert_eq!(quiet.spec.flags, vec!["--allow-read=/tmp/soft".to_string()]);
    }

    #[tokio::test]
    async fn an_ungranted_declaration_contributes_no_flag() {
        let fx = fixture(BTreeMap::new());
        let decl = read_decl("/tmp/loud");
        install(&fx, "acme", tool("t", vec![decl]), "export {}").await;

        let recipe = recipe(&fx, "t").await;
        assert!(
            recipe.spec.flags.is_empty(),
            "a declaration nobody granted reached the flags: {:?}",
            recipe.spec.flags
        );
        // It still prompts: the declaration is what the prompt is matched against.
        let policy = recipe.policy.expect("a known tool has a prompt policy");
        assert_eq!(policy.required.len(), 1);
        assert_eq!(policy.undeclared_policy, UndeclaredPolicy::Deny);
    }

    #[tokio::test]
    async fn a_tool_this_host_cannot_run_gets_no_policy_and_no_terminal() {
        let fx = fixture(BTreeMap::new());
        let mut only_elsewhere = tool("t", vec![read_decl("/tmp/x")]);
        only_elsewhere.platforms = vec!["definitely-not-this-os".to_string()];
        install(&fx, "acme", only_elsewhere, "export {}").await;

        let recipe = recipe(&fx, "t").await;
        assert!(recipe.policy.is_none());
        assert!(recipe.spec.flags.is_empty());
        assert_eq!(recipe.mode, Mode::Pipe);
    }

    #[tokio::test]
    async fn a_known_tool_gets_the_platform_prompting_mode() {
        let fx = fixture(BTreeMap::new());
        install(&fx, "acme", tool("t", Vec::new()), "export {}").await;
        let recipe = recipe(&fx, "t").await;
        assert_eq!(recipe.mode, Mode::for_platform(true));
        assert_ne!(recipe.mode, Mode::Pipe, "a known tool cannot prompt");
    }

    #[tokio::test]
    async fn the_entry_point_comes_from_the_extension_folder() {
        let fx = fixture(BTreeMap::new());
        install(&fx, "acme", tool("t", Vec::new()), "export {}").await;
        let folder = fx.paths.extension_dir("acme");
        std::fs::write(folder.join("deno.json"), r#"{"exports":"./mod.ts"}"#).unwrap();

        let recipe = recipe(&fx, "t").await;
        assert_eq!(recipe.spec.entry_path, folder.join("./mod.ts"));
    }

    #[tokio::test]
    async fn the_home_template_resolves_through_the_injected_environment() {
        // The templates must come from the same environment the worker is
        // given, or `$home` expands to a different path than the one denied.
        let fx = fixture(BTreeMap::from([(
            "HOME".to_string(),
            "/somewhere/else".to_string(),
        )]));
        let decl = read_decl("$home/notes");
        install(&fx, "acme", tool("t", vec![decl.clone()]), "export {}").await;
        grant(&fx, "acme::t", &decl).await;

        let recipe = recipe(&fx, "t").await;
        assert_eq!(
            recipe.spec.flags,
            vec!["--allow-read=/somewhere/else/notes".to_string()]
        );
        let policy = recipe.policy.unwrap();
        assert_eq!(policy.templates.home, PathBuf::from("/somewhere/else"));
        assert_eq!(
            policy.templates.downloads,
            PathBuf::from("/somewhere/else/Downloads")
        );
    }

    #[tokio::test]
    async fn a_write_grant_into_the_install_directory_is_still_refused_here() {
        // The refusal lives in `sandbox`; this is the call site supplying `$extension`.
        let fx = fixture(BTreeMap::new());
        let decl = PermissionDecl::Write {
            optional: None,
            path: "$extension".to_string(),
            reason: "to cache".to_string(),
        };
        install(&fx, "acme", tool("t", vec![decl.clone()]), "export {}").await;
        grant(&fx, "acme::t", &decl).await;

        let recipe = recipe(&fx, "t").await;
        assert!(
            !recipe
                .spec
                .flags
                .iter()
                .any(|flag| flag.starts_with("--allow-write")),
            "the worker may rewrite its own hashed entry file: {:?}",
            recipe.spec.flags
        );
    }

    #[tokio::test]
    async fn the_extension_template_is_this_extension_and_not_the_directory_they_share() {
        // `$extension` resolved one level up would grant a read of every OTHER
        // installed extension's files and bundled credentials.
        let fx = fixture(BTreeMap::new());
        let decl = read_decl("$extension");
        install(&fx, "acme", tool("t", vec![decl.clone()]), "export {}").await;
        grant(&fx, "acme::t", &decl).await;

        let recipe = recipe(&fx, "t").await;
        assert_eq!(
            recipe.spec.flags,
            vec![format!(
                "--allow-read={}",
                fx.paths.extension_dir("acme").display()
            )],
            "the read grant is not scoped to this extension"
        );
    }

    #[tokio::test]
    async fn an_unknown_extension_refuses_before_anything_starts() {
        let fx = fixture(BTreeMap::new());
        plant_deno(&fx.paths, "exit 0");
        let err = fx
            .spawner
            .spawn(&WorkerKey::new("ghost", "t"))
            .await
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ExtensionNotFound);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_spawned_worker_boots_and_its_frames_reach_the_handle() {
        // The whole wiring against a real process on a real terminal; no unit
        // test below would notice this path being unwired.
        let fx = fixture(BTreeMap::new());
        install(&fx, "acme", tool("t", Vec::new()), "export {}").await;
        plant_deno(
            &fx.paths,
            // Announce, then echo whatever Core sends back as a log frame, so
            // the write direction is proven too.
            r#"echo '{"kind":"booted","extensionId":"acme"}'
read line
echo "{\"kind\":\"log\",\"callId\":\"c\",\"level\":\"info\",\"message\":\"seen\"}"
sleep 30"#,
        );

        let handle = spawn_acme(&fx).await;
        let mut events = handle.subscribe();

        tokio::time::timeout(BUDGET, handle.wait_for_boot(BUDGET))
            .await
            .expect("boot hung")
            .expect("the worker never booted");

        handle.send(crate::extensions::worker::protocol::PoolToWorker::Shutdown);
        let event = tokio::time::timeout(BUDGET, events.recv())
            .await
            .expect("no frame came back")
            .expect("the worker channel closed");
        match event {
            crate::extensions::worker::protocol::WorkerEvent::Frame(frame) => {
                assert!(
                    format!("{frame:?}").contains("seen"),
                    "the worker did not receive what Core wrote: {frame:?}"
                );
            }
            other => panic!("expected the worker's answer, got {other:?}"),
        }
        handle.terminate(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pty_workers_terminal_output_reaches_the_driver() {
        // The terminal is the ONLY channel a Deno permission prompt arrives on;
        // not forwarding it hangs every ask-state permission silently.
        let fx = fixture(BTreeMap::new());
        install(&fx, "acme", tool("t", Vec::new()), "export {}").await;
        plant_deno(
            &fx.paths,
            r#"echo '{"kind":"booted","extensionId":"acme"}'
echo 'the tool said something' >&2
sleep 30"#,
        );

        let handle = spawn_acme(&fx).await;
        let mut events = handle.subscribe();
        let line = loop {
            match tokio::time::timeout(BUDGET, events.recv())
                .await
                .expect("nothing arrived off the terminal")
                .expect("the worker channel closed")
            {
                crate::extensions::worker::protocol::WorkerEvent::StderrLog { line } => break line,
                _ => continue,
            }
        };
        assert!(line.contains("the tool said something"), "got {line:?}");
        handle.terminate(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_worker_that_dies_before_booting_fails_its_waiter() {
        let fx = fixture(BTreeMap::new());
        install(&fx, "acme", tool("t", Vec::new()), "export {}").await;
        plant_deno(&fx.paths, "exit 3");

        let handle = spawn_acme(&fx).await;
        let err = tokio::time::timeout(BUDGET, handle.wait_for_boot(BUDGET))
            .await
            .expect("the boot wait hung")
            .expect_err("a dead worker reported a successful boot");
        assert!(
            err.message.contains("exited before it booted"),
            "the exit was not what failed the waiter: {}",
            err.message
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pipe_mode_worker_still_reaches_its_worker() {
        // Pipe mode has no terminal but still needs a write channel (stdin),
        // or no frame ever reaches the worker.
        let fx = fixture(BTreeMap::new());
        plant_deno(
            &fx.paths,
            r#"read line
echo "{\"kind\":\"log\",\"callId\":\"c\",\"level\":\"info\",\"message\":\"heard\"}"
sleep 30"#,
        );
        let launched = launch_piped(&fx).await;

        let mut incoming = launched.incoming;
        launched.terminal.write(b"{\"kind\":\"ping\"}\n");
        let line = loop {
            match tokio::time::timeout(BUDGET, incoming.recv())
                .await
                .expect("nothing came back")
                .expect("the worker channel closed")
            {
                Incoming::ProtocolLine(line) => break line,
                _ => continue,
            }
        };
        assert!(line.contains("heard"), "unexpected frame: {line}");
        launched.terminal.kill();
        // The kill has to produce an exit, or a wedged worker is never reaped.
        loop {
            match tokio::time::timeout(BUDGET, incoming.recv())
                .await
                .expect("the killed worker never reported an exit")
            {
                Some(Incoming::Exited { .. }) | None => break,
                Some(_) => continue,
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pipe_mode_workers_stderr_arrives_as_terminal_text() {
        let fx = fixture(BTreeMap::new());
        plant_deno(&fx.paths, "echo 'a word from the tool' >&2\nsleep 30");
        let launched = launch_piped(&fx).await;

        let mut incoming = launched.incoming;
        let text = loop {
            match tokio::time::timeout(BUDGET, incoming.recv())
                .await
                .expect("no stderr arrived")
                .expect("the worker channel closed")
            {
                Incoming::Terminal(bytes) => break String::from_utf8_lossy(&bytes).into_owned(),
                _ => continue,
            }
        };
        assert!(text.contains("a word from the tool"), "got {text:?}");
        launched.terminal.kill();
    }
}
