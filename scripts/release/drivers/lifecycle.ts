// Resource lifecycle for build-environment drivers.
//
// Priority constraint (non-negotiable): no dangling resource on failure for any
// reason, and start/stop only as needed. withEnvironment() brings an environment
// up immediately before its work and tears it down immediately after - on
// success, on a thrown error, and on Ctrl-C - while never stopping an
// environment the user already had running.

import { colors, info } from "../lib.ts";
import type { BuildEnvironment } from "./mod.ts";

// Every environment this process started and has not yet torn down. The signal
// backstop walks this set so an interrupt stops everything synchronously.
const started = new Set<BuildEnvironment>();
let signalsInstalled = false;

function installSignalBackstop(): void {
  if (signalsInstalled) return;
  signalsInstalled = true;
  const onSignal = () => {
    for (const env of started) {
      try {
        env.teardownSync();
      } catch {
        // best-effort: a failed teardown of one env must not block the others
      }
    }
    started.clear();
    Deno.exit(130);
  };
  Deno.addSignalListener("SIGINT", onSignal);
  Deno.addSignalListener("SIGTERM", onSignal);
}

/** Run `fn` with `env` up, guaranteeing teardown afterwards. Restores prior
 *  state: if the user already had the environment running, it is left running.
 *  Only what this run started is stopped, and it is stopped even if `fn` throws
 *  or the process is interrupted. */
export async function withEnvironment<T>(env: BuildEnvironment, fn: () => Promise<T>): Promise<T> {
  installSignalBackstop();
  const prior = await env.detectState();
  const weStartIt = prior !== "RUNNING";
  if (!weStartIt) {
    info(`reusing already-running ${env.id}`);
    return await fn();
  }
  info(`starting ${env.id} (was ${prior.toLowerCase()})`);
  // Register BEFORE ensureUp so a failure mid-start (e.g. utmctl started the VM
  // but SSH never came up) is still torn down by the finally below + the signal
  // backstop. ensureUp is inside the try for the same reason. Build methods throw
  // (never fail()/Deno.exit), so the finally always runs on failure.
  started.add(env);
  try {
    await env.ensureUp();
    return await fn();
  } finally {
    // LOCA_KEEP_BUILD_ENVS leaves environments this run started RUNNING after it
    // finishes, so a sequence of local builds reuses one warm VM/machine instead
    // of booting + tearing down each time (much faster iteration). The caller is
    // then responsible for stopping them. Off by default: a normal release
    // restores prior state.
    if (keepEnvsAlive()) {
      info(colors.yellow(`leaving ${env.id} running (LOCA_KEEP_BUILD_ENVS)`));
      started.delete(env);
    } else {
      try {
        await env.teardown();
      } catch (err) {
        info(colors.yellow(`teardown of ${env.id} failed: ${err}`));
        // Fall back to the sync path so the resource is not left running.
        try {
          env.teardownSync();
        } catch {
          /* best-effort */
        }
      }
      started.delete(env);
    }
  }
}

function keepEnvsAlive(): boolean {
  const v = Deno.env.get("LOCA_KEEP_BUILD_ENVS");
  return v === "1" || v === "true";
}
