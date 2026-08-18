//! Argv dispatcher for the install and uninstall subcommands.
//!
//! Ports `install/cli.ts`. The desktop frontend calls this before booting the
//! server: when `argv[0]` names a verb, the core binary acts as a short-lived
//! installer CLI and exits with the code this returns; otherwise it returns
//! `None` and the server boots. That is what lets every installer front-end
//! wrap ONE implementation of service registration, secret bootstrap and
//! pairing.
//!
//! # `None` means "boot the server", so an unrecognised verb is not inert
//!
//! This is the trap that decides the whole shape of the file. Returning `None`
//! is not "do nothing": the caller starts a core. So a dispatcher that failed
//! to recognise `start-service` would not report an error, it would **boot a
//! second core** when the Client asked it to start one. `VERBS` is therefore
//! the single source of truth for both halves of the decision, and the test
//! that pins it walks the same list rather than restating it.
//!
//! The `None` is also load-bearing in the other direction: launchd starts the
//! binary with EMPTY argv, and the updater relaunches it with
//! `--restart-args`, so both must fall through to a normal boot.
//!
//! # It never fails, it returns 1
//!
//! `maybe_run` propagates no error. A failing verb prints the reason as
//! progress and returns exit code 1 (`cli.ts:108-111`), because the caller is
//! an installer hook, a package manager or the Client, and a Rust panic or a
//! propagated error there is an install that fails with no explanation on
//! stderr.
//!
//! # The flags
//!
//! Parsed as a SET of `--`-prefixed arguments, position-independent, with
//! unknown flags ignored (`cli.ts:63`). That tolerance is a compatibility
//! promise: `core-installers.ts` passes `--keep-data` for `apt remove` and
//! omits it for purge, and a future front-end passing a flag this build does
//! not know must not fail the uninstall.

use crate::error::Result;
use crate::update::install::bootstrap::{bootstrap, enable_behind_proxy, BootstrapOptions};
use crate::update::install::fetch_verify::self_install;
use crate::update::install::pair::{mint_code, MintTimings};
use crate::update::install::service::{
    install_service, restart_service, start_service, stop_service, uninstall_service, ServiceCtx,
};
use crate::update::net::Net;

/// Every verb this binary answers to.
///
/// One list, used BOTH to decide whether argv is a subcommand and to dispatch
/// it. Two lists is how a verb becomes recognised but undispatched, or
/// dispatched but unrecognised, and the second of those boots a server.
pub const VERBS: &[&str] = &[
    "self-install",
    "bootstrap",
    "install-service",
    "start-service",
    "stop-service",
    "mint-code",
    "uninstall-service",
    "enable-behind-proxy",
];

/// The four flags, after the environment has had its say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub bind_all: bool,
    pub behind_proxy: bool,
    pub keep_data: bool,
    pub purge_models: bool,
}

/// Everything the verbs need, assembled by the frontend.
///
/// A parameter rather than built in here, so `maybe_run` itself is testable
/// end to end and the only untested wiring left is the frontend's declarative
/// construction of these three.
pub struct InstallCli<'a> {
    pub ctx: ServiceCtx<'a>,
    pub net: &'a Net,
    pub timings: MintTimings,
}

/// Read the flags, letting the environment turn on what a flag would.
///
/// `--bind-all` and `LOCA_INSTALL_BIND_ALL=1` are alternatives, not a
/// precedence: either turns it on (`cli.ts:64-66`). The unified installers set
/// the variable; the Client passes the flag.
pub fn parse_flags(args: &[String], env_bind_all: bool, env_behind_proxy: bool) -> Flags {
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    Flags {
        bind_all: has("--bind-all") || env_bind_all,
        behind_proxy: has("--behind-proxy") || env_behind_proxy,
        keep_data: has("--keep-data"),
        purge_models: has("--purge-models"),
    }
}

/// Run an install subcommand if `args` names one.
///
/// `Some(code)` is the process exit code. `None` means argv is not a
/// subcommand and the caller should boot the server.
pub async fn maybe_run(args: &[String], cli: &InstallCli<'_>) -> Option<i32> {
    let verb = args.first()?;
    if !VERBS.contains(&verb.as_str()) {
        return None;
    }
    let flags = parse_flags(&args[1..], cli.ctx.env.bind_all, cli.ctx.env.behind_proxy);
    match dispatch(verb, flags, cli).await {
        Ok(()) => Some(0),
        Err(err) => {
            // Never propagated. The caller is an installer hook or the Client,
            // and the reason has to reach a human on stderr.
            cli.ctx.io.progress(&format!("{verb} failed: {err}"));
            Some(1)
        }
    }
}

async fn dispatch(verb: &str, flags: Flags, cli: &InstallCli<'_>) -> Result<()> {
    let ctx = &cli.ctx;
    let options = BootstrapOptions {
        bind_all: flags.bind_all,
        behind_proxy: flags.behind_proxy,
    };
    match verb {
        "self-install" => self_install(ctx.config, ctx.paths, cli.net, ctx.io).await,
        "bootstrap" => bootstrap(ctx.config, ctx.paths, cli.net, ctx.io, options).await,
        "install-service" => {
            // Bootstrap first, and it is idempotent, so one call from an
            // installer post-install hook fully provisions the daemon whether
            // or not this is an upgrade.
            bootstrap(ctx.config, ctx.paths, cli.net, ctx.io, options).await?;
            install_service(ctx).await
        }
        "start-service" => start_service(ctx).await,
        "stop-service" => stop_service(ctx).await,
        "mint-code" => mint_code(ctx.config, ctx.paths, ctx.io, cli.timings)
            .await
            .map(|_| ()),
        "uninstall-service" => uninstall_service(ctx, flags.keep_data, flags.purge_models).await,
        "enable-behind-proxy" => {
            // Its own verb rather than an install-time option, because a
            // proxy-served core folds no certificate pin and so cannot be
            // paired over loopback. The operator pairs first and flips after,
            // and the restart is what makes the setting take effect.
            enable_behind_proxy(ctx.paths, ctx.io).await?;
            restart_service(ctx).await
        }
        // Unreachable: `maybe_run` checked membership against the same list.
        // Returning Ok here rather than panicking keeps the "never throws"
        // contract true even if the two ever drift.
        _ => Ok(()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::{Channel, Config};
    use crate::paths::Paths;
    use crate::update::install::io::testing::{recording, Captured};
    use crate::update::install::io::Streams;
    use crate::update::install::proc::testing::RecordingRunner;
    use crate::update::install::proc::{RunResult, Runner};
    use crate::update::install::service::{InstallEnv, MasterKey};
    use crate::update::install::supervisor::Os;
    use crate::update::testing::bounded;

    struct NoKey;

    #[async_trait::async_trait]
    impl MasterKey for NoKey {
        async fn delete(&self) -> Result<()> {
            Ok(())
        }
    }

    struct Env {
        _dir: tempfile::TempDir,
        paths: Paths,
        home: std::path::PathBuf,
        config: Config,
        io: Streams<Captured, Captured>,
        out: Captured,
        err: Captured,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path().join("core"));
        paths.ensure_dirs().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let (io, out, err) = recording();
        Env {
            _dir: dir,
            paths,
            home,
            config: Config {
                channel: Channel::Stable,
                host: "127.0.0.1".into(),
                // Port 1 answers nothing, so mint-code fails fast rather than
                // reaching a real listener.
                port: 1,
                version: "0.1.15",
                storage_base_url: "https://127.0.0.1:1".into(),
                hf_base_url: "https://127.0.0.1:1".into(),
            },
            io,
            out,
            err,
        }
    }

    fn cli<'a>(
        e: &'a Env,
        runner: &'a dyn Runner,
        key: &'a dyn MasterKey,
        net: &'a Net,
        install_env: InstallEnv,
    ) -> InstallCli<'a> {
        InstallCli {
            ctx: ServiceCtx {
                config: &e.config,
                paths: &e.paths,
                runner,
                io: &e.io,
                master_key: key,
                os: Os::Macos,
                home: &e.home,
                self_pid: 4242,
                current_exe: e.paths.bin_path("loca-core"),
                env: install_env,
            },
            net,
            timings: MintTimings {
                health_attempts: 1,
                mint_attempts: 1,
                probe_timeout: std::time::Duration::from_millis(50),
                retry_delay: std::time::Duration::from_millis(1),
            },
        }
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[tokio::test]
    async fn empty_argv_boots_the_server() {
        // launchd starts the binary with NO arguments. Returning anything but
        // `None` here means a service install never starts a core at all.
        let e = env();
        let runner = RecordingRunner::new();
        let net = Net::new().unwrap();
        let c = cli(&e, &runner, &NoKey, &net, InstallEnv::default());

        assert_eq!(bounded(maybe_run(&argv(&[]), &c)).await, None);
        assert_eq!(bounded(maybe_run(&argv(&[""]), &c)).await, None);
        assert!(runner.calls().is_empty(), "and nothing was run");
    }

    #[tokio::test]
    async fn the_updater_relaunch_arguments_fall_through_to_a_boot() {
        // The updater relaunches core with its own arguments. If any of these
        // were mistaken for a verb, a self-update would end with a core that
        // ran an installer subcommand and exited instead of coming back.
        let e = env();
        let runner = RecordingRunner::new();
        let net = Net::new().unwrap();
        let c = cli(&e, &runner, &NoKey, &net, InstallEnv::default());

        for args in [
            vec!["--restart-args"],
            vec!["--restart-args", "start-service"],
            vec!["serve"],
            vec!["--help"],
            vec!["install"],
            vec!["startservice"],
            vec!["start-service-x"],
            vec!["START-SERVICE"],
        ] {
            assert_eq!(
                bounded(maybe_run(&argv(&args), &c)).await,
                None,
                "{args:?} must boot the server"
            );
        }
    }

    #[tokio::test]
    async fn every_declared_verb_is_recognised_and_none_boots_the_server() {
        // Walks VERBS rather than restating it, so a verb added to the list
        // without a dispatch arm is caught here rather than by booting a second
        // core in production. This is the D-100 question asked of a dispatcher:
        // the list and the match must not be able to drift.
        let e = env();
        let runner = RecordingRunner::new()
            .on("launchctl", RunResult::ok(""))
            .on("systemctl", RunResult::ok(""))
            .on("sh -c", RunResult::ok("101\n"))
            .on("pgrep", RunResult::failed(1));
        let net = Net::new().unwrap();
        let c = cli(&e, &runner, &NoKey, &net, InstallEnv::default());

        for verb in VERBS {
            let code = bounded(maybe_run(&argv(&[verb]), &c)).await;
            assert!(
                code.is_some(),
                "{verb} returned None, which BOOTS A SERVER instead of running the verb"
            );
        }
    }

    #[tokio::test]
    async fn a_failing_verb_returns_one_and_says_why_rather_than_propagating() {
        // The caller is an installer hook or the Client. A propagated error is
        // an install that fails with nothing on stderr.
        let e = env();
        // Nothing is scripted, so every command fails.
        let runner = RecordingRunner::new();
        let net = Net::new().unwrap();
        let c = cli(&e, &runner, &NoKey, &net, InstallEnv::default());

        let code = bounded(maybe_run(&argv(&["start-service"]), &c)).await;

        assert_eq!(code, Some(1));
        assert!(
            e.err.text().contains("start-service failed:"),
            "{}",
            e.err.text()
        );
        assert_eq!(e.out.text(), "", "a failure puts nothing on stdout");
    }

    #[tokio::test]
    async fn install_service_bootstraps_first() {
        // One call from a post-install hook must fully provision the daemon.
        // Registering a service against a core with no admin token leaves an
        // install that runs and cannot be paired.
        let e = env();
        let runner = RecordingRunner::new().on("launchctl", RunResult::ok(""));
        let net = Net::new().unwrap();
        let c = cli(&e, &runner, &NoKey, &net, InstallEnv::default());

        let code = bounded(maybe_run(&argv(&["install-service", "--bind-all"]), &c)).await;

        assert_eq!(code, Some(0));
        assert!(
            !std::fs::read_to_string(e.paths.admin_token_file())
                .unwrap()
                .is_empty(),
            "the token is minted before the service is registered"
        );
        assert!(std::fs::read_to_string(e.paths.settings_file())
            .unwrap()
            .contains("0.0.0.0"));
        assert!(runner.calls().iter().any(|c| c.contains("launchctl load")));
    }

    /// A runner that records the state of `settings.json` at the moment each
    /// command was issued.
    ///
    /// The ordering this file cares about spans two different sinks: the
    /// setting is a FILE and the restart is a COMMAND, so neither the recorded
    /// argv nor the file contents can see the order on its own. Sampling the
    /// file when the command runs is what puts both on one timeline.
    struct OrderingRunner {
        settings: std::path::PathBuf,
        seen: std::sync::Mutex<Vec<(String, bool)>>,
    }

    impl OrderingRunner {
        fn new(settings: std::path::PathBuf) -> Self {
            Self {
                settings,
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<(String, bool)> {
            self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    #[async_trait::async_trait]
    impl Runner for OrderingRunner {
        async fn run(&self, argv: &[&str]) -> RunResult {
            let flipped = std::fs::read_to_string(&self.settings)
                .map(|raw| raw.contains("\"server.behindProxy\": true"))
                .unwrap_or(false);
            self.seen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((argv.join(" "), flipped));
            RunResult::ok("")
        }
    }

    #[tokio::test]
    async fn enable_behind_proxy_flips_the_setting_then_restarts() {
        // The order is the point: restarting first restarts a core that has not
        // read the new setting, so the operator flips the switch and nothing
        // happens until the next restart.
        //
        // Asserting only that BOTH happened is not enough, and that is not
        // hypothetical: swapping the two survived the first version of this
        // test. What pins it is sampling settings.json at the moment the
        // restart command is issued.
        let e = env();
        let runner = OrderingRunner::new(e.paths.settings_file());
        let net = Net::new().unwrap();
        let c = cli(&e, &runner, &NoKey, &net, InstallEnv::default());

        let code = bounded(maybe_run(&argv(&["enable-behind-proxy"]), &c)).await;

        assert_eq!(code, Some(0));
        assert!(std::fs::read_to_string(e.paths.settings_file())
            .unwrap()
            .contains("server.behindProxy"));
        let seen = runner.seen();
        assert!(!seen.is_empty(), "the core was never restarted");
        for (argv, flipped) in &seen {
            assert!(
                *flipped,
                "{argv} ran BEFORE the setting was written, so the restarted \
                 core never reads it"
            );
        }
    }

    #[tokio::test]
    async fn uninstall_reads_its_two_flags_in_any_position() {
        // `core-installers.ts` passes --keep-data for `apt remove` and omits it
        // for purge, and the unified uninstaller offers the two as separate
        // choices. Position-independent, because no front-end promises an order.
        let e = env();
        std::fs::create_dir_all(e.paths.models_dir()).unwrap();
        let runner = RecordingRunner::new().on("launchctl", RunResult::ok(""));
        let net = Net::new().unwrap();
        let c = cli(&e, &runner, &NoKey, &net, InstallEnv::default());

        let code = bounded(maybe_run(
            &argv(&["uninstall-service", "--purge-models", "--keep-data"]),
            &c,
        ))
        .await;

        assert_eq!(code, Some(0));
        assert!(e.paths.root().exists(), "--keep-data kept the data");
        assert!(
            !e.paths.models_dir().exists(),
            "--purge-models purged regardless"
        );
    }

    #[test]
    fn the_flags_are_a_set_and_an_unknown_one_is_ignored() {
        // A future front-end passing a flag this build does not know must not
        // fail the uninstall.
        let flags = parse_flags(
            &argv(&["--purge-models", "--from-a-newer-installer", "--keep-data"]),
            false,
            false,
        );
        assert!(flags.keep_data);
        assert!(flags.purge_models);
        assert!(!flags.bind_all);

        // The environment is an ALTERNATIVE to the flag, not a precedence:
        // either turns it on. The unified installers set the variable, the
        // Client passes the flag, and an install driven by both must work.
        assert!(parse_flags(&argv(&["--bind-all"]), false, false).bind_all);
        assert!(parse_flags(&argv(&[]), true, false).bind_all);
        assert!(parse_flags(&argv(&["--bind-all"]), true, false).bind_all);
        assert!(!parse_flags(&argv(&[]), false, false).bind_all);
        assert!(parse_flags(&argv(&[]), false, true).behind_proxy);

        // A verb is not a flag, and a bare word is not one either: only
        // `--`-prefixed arguments count.
        assert!(!parse_flags(&argv(&["keep-data"]), false, false).keep_data);
        assert!(!parse_flags(&argv(&["-keep-data"]), false, false).keep_data);
    }

    #[tokio::test]
    async fn the_verb_is_only_read_from_the_first_argument() {
        // A path or a value that happens to equal a verb must not turn a boot
        // into an uninstall. `cli.ts:60` reads args[0] and nothing else.
        let e = env();
        let runner = RecordingRunner::new();
        let net = Net::new().unwrap();
        let c = cli(&e, &runner, &NoKey, &net, InstallEnv::default());

        assert_eq!(
            bounded(maybe_run(&argv(&["--flag", "uninstall-service"]), &c)).await,
            None
        );
        assert!(e.paths.root().exists(), "nothing was uninstalled");
    }

    #[tokio::test]
    async fn no_verb_makes_an_outbound_call_it_was_not_asked_to() {
        // The consent rule at the dispatcher. Only the three install-phase
        // verbs may reach the network, and `start`, `stop` and
        // `uninstall-service` are exactly where a "while we are here, check for
        // an update" call would later be added.
        let e = env();
        let runner = RecordingRunner::new()
            .on("launchctl", RunResult::ok(""))
            .on("pgrep", RunResult::failed(1));
        let net = Net::new().unwrap();
        let c = cli(&e, &runner, &NoKey, &net, InstallEnv::default());

        for verb in ["start-service", "stop-service", "uninstall-service"] {
            let _ = bounded(maybe_run(&argv(&[verb, "--keep-data"]), &c)).await;
        }

        assert_eq!(net.calls(), 0);
    }
}
