//! The desktop frontend: resolve config, install logging, boot, serve.
//!
//! Mobile does not go through here. It links the library directly via
//! `loca-core-ffi` and drives the router in-process, which is why everything
//! below the entry point lives in the library rather than in this file.

use std::process::ExitCode;

use loca_core::paths::Paths;
use loca_core::{boot, log, Config};

fn main() -> ExitCode {
    // Config and paths resolve before logging, so these two are the only place
    // stderr is written directly. Everything after goes through the logger.
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("boot failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let paths = match Paths::from_env(config.channel) {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("boot failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    log::init(Some(&paths.logs_dir()));
    tracing::info!(
        target: "loca::boot",
        "Loca Core {} starting (channel={}, root={})",
        config.version,
        config.channel,
        paths.root().display(),
    );

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!(target: "loca::boot", "could not start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(async move {
        // The install CLI, BEFORE `boot`. Without this the whole `install/`
        // subtree is unreachable: it is what the NSIS installer, the macOS pkg
        // postinstall and the deb/rpm hooks all invoke as `install-service`,
        // what the Client runs as `start-service` / `stop-service`, and what
        // every uninstaller runs.
        //
        // **The `None` arm is as load-bearing as the `Some`.** `maybe_run`
        // answers `None` when argv is not a subcommand, and the caller must then
        // boot normally: launchd starts this binary with EMPTY argv, and the
        // updater relaunches it with `--restart-args`. A frontend that treated
        // `None` as "nothing to do" would give a service install that never
        // starts a core, and a self-update that never comes back.
        //
        // `ServiceCtx` takes the platform facts as DATA (the OS, the real home,
        // this pid, this executable) so every branch is exercisable on one host.
        // This construction is the only place they are read for real, and is
        // therefore the untested wiring in this file.
        let args: Vec<String> = std::env::args().skip(1).collect();
        let home = match loca_core::update::install::proc::real_home() {
            Ok(home) => home,
            Err(e) => {
                tracing::error!(target: "loca::boot", "could not resolve the home directory: {e}");
                return ExitCode::FAILURE;
            }
        };
        let current_exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                tracing::error!(target: "loca::boot", "could not resolve this executable: {e}");
                return ExitCode::FAILURE;
            }
        };
        let net = match loca_core::update::net::Net::new() {
            Ok(net) => net,
            Err(e) => {
                tracing::error!(target: "loca::boot", "could not build the HTTP client: {e}");
                return ExitCode::FAILURE;
            }
        };
        let cli = loca_core::update::install::cli::InstallCli {
            ctx: loca_core::update::install::service::ServiceCtx {
                config: &config,
                paths: &paths,
                runner: &loca_core::update::install::proc::SystemRunner,
                io: &loca_core::update::install::io::Streams::std(),
                master_key: &loca_core::update::install::service::VaultMasterKey {
                    paths: &paths,
                    channel: config.channel,
                },
                os: loca_core::update::install::supervisor::Os::host(),
                // `real_home()`, NOT `paths.root()`: service definitions must
                // land in the real `~/Library` or `~/.config` even when
                // `LOCA_CORE_HOME` redirects core state to a tempdir.
                home: &home,
                self_pid: std::process::id(),
                current_exe,
                env: loca_core::update::install::service::InstallEnv::from_env(),
            },
            net: &net,
            timings: loca_core::update::install::pair::MintTimings::SHIPPING,
        };
        if let Some(code) = loca_core::update::install::cli::maybe_run(&args, &cli).await {
            return ExitCode::from(code as u8);
        }

        let booted = match boot::boot(config, paths).await {
            Ok(booted) => booted,
            Err(e) => {
                tracing::error!(target: "loca::boot", "boot failed: {e}");
                return ExitCode::FAILURE;
            }
        };

        // Ctrl-C stops accepting, drains in-flight requests, and closes every
        // client connection, so a restart does not leave sockets hanging on a
        // core that is already gone.
        let booted = std::sync::Arc::new(booted);
        let signalled = std::sync::Arc::clone(&booted);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!(target: "loca::boot", "shutting down");
                // Order matters and runs outside-in. MCP closes its own
                // sessions first, because a connection torn down under it logs
                // a transport error rather than a clean close, and its stdio
                // children are registered with the sidecar manager that the
                // next line reaps. Children before the listener: once the
                // listener closes there is nothing left to run the graceful
                // kill, and only the supervisor backstop (the systemd cgroup,
                // the launchd process group, the Windows Job Object) reaps.
                // The scheduler first: it is the only subsystem that STARTS
                // work rather than serving it, so stopping the arm loop before
                // anything else means no new turn begins during the teardown
                // below.
                signalled.scheduled.shutdown();
                // Warm extension workers before the manager: a warm worker is a
                // live `deno`, and the manager's Job Object is a backstop for a
                // hard kill rather than the graceful path. PTY-mode workers are
                // not in `children` at all, so the manager would not reap them.
                if let Some(pool) = signalled.extension_pool.as_ref() {
                    pool.shutdown().await;
                }
                signalled.mcp.shutdown().await;
                signalled.sidecars.shutdown().await;
                signalled.core.shutdown();
            }
        });

        match booted.serve().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!(target: "loca::boot", "serve failed: {e}");
                ExitCode::FAILURE
            }
        }
    })
}
