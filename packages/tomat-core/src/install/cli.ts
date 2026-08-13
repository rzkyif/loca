// Argv dispatcher for the install/uninstall subcommands.
//
// main.ts calls this before booting the server: when argv[0] is one of the
// known verbs the core binary acts as a short-lived installer CLI and exits;
// otherwise (empty argv from launchd/systemd, or the updater's --restart-args)
// it returns null and the server boots. This is what lets every installer
// front-end - the native installers' install and uninstall hooks - wrap ONE
// implementation of service registration, secret bootstrap, and pairing.
//
// Verbs:
//   self-install       fetch + verify + place the binary set from core.json
//   bootstrap          admin token, optional bind-all seed, plant built-in ext
//   install-service    bootstrap, then register + start the OS service
//   start-service      start the core through its supervisor; print one JSON
//                      line: { supervisor, pid } (pid only in background mode)
//   stop-service       stop the core through its supervisor
//   mint-code          print one JSON line: { code, url, port }
//   uninstall-service  stop + remove the service, keychain key, and data
//   enable-behind-proxy  turn on server.behindProxy on a paired core + restart
//
// Env honored: TOMAT_CHANNEL (channel selection, via paths.ts),
// TOMAT_INSTALL_SERVICE (0 = background, no service; also selects the
// enable-behind-proxy restart path), TOMAT_INSTALL_BIND_ALL (1 = seed
// server.bindHost=0.0.0.0), TOMAT_INSTALL_BEHIND_PROXY (1 = seed
// server.behindProxy=true for a core served through an HTTPS proxy),
// TOMAT_INSTALL_REGISTER_ARP (0 = skip the Add/Remove Programs entry; the
// unified Windows installer sets this because its own uninstall.exe owns the
// Core's ARP entry). Flags: --keep-data / --purge-models (uninstall),
// --bind-all / --behind-proxy (bootstrap / install-service, same as the
// matching env var).

import { errMessage } from "@tomat/shared";
import { bootstrap, enableBehindProxy } from "./bootstrap.ts";
import {
  installService,
  restartService,
  startService,
  stopService,
  uninstallService,
} from "./service.ts";
import { mintCode } from "./pair.ts";
import { selfInstall } from "./fetch-verify.ts";
import { progress } from "./io.ts";

const VERBS = new Set([
  "self-install",
  "bootstrap",
  "install-service",
  "start-service",
  "stop-service",
  "mint-code",
  "uninstall-service",
  "enable-behind-proxy",
]);

/** Run an install subcommand if argv names one. Returns the process exit code
 *  to use, or null when argv is not a subcommand (server should boot). Never
 *  throws: a failing subcommand returns exit code 1. */
export async function maybeRunInstallCommand(args: string[]): Promise<number | null> {
  const verb = args[0];
  if (!verb || !VERBS.has(verb)) return null;

  const flags = new Set(args.slice(1).filter((a) => a.startsWith("--")));
  const bindAll = flags.has("--bind-all") || Deno.env.get("TOMAT_INSTALL_BIND_ALL") === "1";
  const behindProxy =
    flags.has("--behind-proxy") || Deno.env.get("TOMAT_INSTALL_BEHIND_PROXY") === "1";
  const keepData = flags.has("--keep-data");
  const purgeModels = flags.has("--purge-models");

  try {
    switch (verb) {
      case "self-install":
        await selfInstall();
        break;
      case "bootstrap":
        await bootstrap({ bindAll, behindProxy });
        break;
      case "install-service":
        // Bootstrap (idempotent) then register the service, so a single call
        // from an installer post-install hook fully provisions the daemon.
        await bootstrap({ bindAll, behindProxy });
        await installService();
        break;
      case "start-service":
        await startService();
        break;
      case "stop-service":
        await stopService();
        break;
      case "mint-code":
        await mintCode();
        break;
      case "uninstall-service":
        await uninstallService({ keepData, purgeModels });
        break;
      case "enable-behind-proxy":
        // Turn on server.behindProxy for an already-installed core and restart
        // it so the setting takes effect. Kept as its own verb (rather than an
        // install-time-only option) so an operator can pair over loopback
        // first and flip afterwards: a proxy-served core folds no cert pin, so
        // it cannot be paired over loopback. An already-pinned loopback client
        // survives the restart (same key, same cert served on loopback).
        await enableBehindProxy();
        await restartService();
        break;
    }
    return 0;
  } catch (err) {
    progress(`${verb} failed: ${errMessage(err)}`);
    return 1;
  }
}
