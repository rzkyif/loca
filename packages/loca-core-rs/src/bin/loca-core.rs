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
        // The install CLI, BEFORE `boot`: the native installers invoke it as
        // `install-service`, the Client as `start-service` / `stop-service`.
        //
        // **The `None` arm is as load-bearing as the `Some`.** `maybe_run`
        // answers `None` when argv is not a subcommand, and the caller must
        // then boot normally: launchd starts this binary with EMPTY argv, and
        // the updater relaunches it with `--restart-args`.
        //
        // `ServiceCtx` takes the platform facts as data, so every branch is
        // exercisable on one host; this construction is the only place they are
        // read for real.
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
                // `LOCA_CORE_HOME` redirects core state elsewhere.
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

        // Before `boot`, so it runs ahead of anything a broken new binary
        // could fail at. A self-update writes an anchor and a marker, and this
        // is the only thing that reads them: without it a core that updated
        // into a binary that cannot start crash-loops with its own recovery
        // sitting unused on disk.
        //
        // **Exit 75, not 0.** systemd's `Restart=on-failure` and Task
        // Scheduler's restart-on-failure only relaunch on a failure exit, so a
        // clean exit would leave the rolled-back binary stopped.
        if loca_core::update::rollback::handle_update_marker_on_boot(&config, &paths).await {
            tracing::warn!(
                target: "loca::update",
                "rolled back to the previous core; exiting 75 so the supervisor relaunches it",
            );
            return ExitCode::from(75);
        }

        // Also before `boot`, and deliberately NOT inside it: `boot` is shared
        // with the mobile embed, which has no updater binary at all, so a check
        // in there would make every mobile core refuse to start.
        //
        // Refuse rather than degrade: a core with no `loca-core-updater` starts
        // happily and discovers mid-update that nothing can relaunch it. It
        // also repairs the one state `swap::commit_rename` can leave behind, a
        // hard kill between its two renames, which nothing else restores
        // because the process that would is the one that died.
        if let Err(e) = loca_core::update::helpers::ensure_helper_binaries(&config, &paths).await {
            tracing::error!(target: "loca::boot", "boot failed: {e}");
            return ExitCode::FAILURE;
        }

        let booted = match boot::boot(config, paths).await {
            Ok(booted) => booted,
            Err(e) => {
                tracing::error!(target: "loca::boot", "boot failed: {e}");
                return ExitCode::FAILURE;
            }
        };

        // Ctrl-C stops accepting, drains in-flight requests, and closes every
        // client connection.
        let booted = std::sync::Arc::new(booted);
        let signalled = std::sync::Arc::clone(&booted);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!(target: "loca::boot", "shutting down");
                // Order matters, outside-in. The scheduler first: it is the
                // only subsystem that STARTS work, so stopping its arm loop
                // means no new turn begins during the teardown. Then MCP, since
                // a connection torn down under it logs a transport error rather
                // than a clean close and its stdio children are the manager's.
                // Children before the listener: once the listener closes,
                // nothing is left to run the graceful kill.
                signalled.scheduled.shutdown();
                // Warm extension workers before the manager: PTY-mode workers
                // are not in `children`, so the manager would not reap them.
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
