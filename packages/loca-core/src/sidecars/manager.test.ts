// Integration test for SidecarManager. Spawns a fake HTTP sidecar
// (tests/fixtures/sidecars/http-stub.ts) via the real Deno binary and
// drives the manager through its lifecycle.

import { assert, assertEquals } from "@std/assert";
import { fromFileUrl } from "@std/path";
import { __resetForTesting, sidecarManager } from "./manager.ts";
import type { SidecarSnapshot } from "./types.ts";
import { initLogger } from "../shared/log.ts";
import { freePort } from "../../tests/helpers/free-port.ts";

// Isolate core paths (incl. the logger's core.log) to a tempdir so this
// integration test never writes into the developer's real ~/.loca.
Deno.env.set("LOCA_CORE_HOME", await Deno.makeTempDir({ prefix: "loca-sidecar-test-" }));
await initLogger();

const STUB_PATH = fromFileUrl(
  new URL("../../tests/fixtures/sidecars/http-stub.ts", import.meta.url),
);

function buildOpts(port: number) {
  return {
    binary: Deno.execPath(),
    args: ["run", `--allow-net=127.0.0.1:${port}`, STUB_PATH, String(port)],
    readiness: {
      kind: "http" as const,
      url: `http://127.0.0.1:${port}/health`,
    },
    startupTimeoutMs: 10_000,
    restartPolicy: "none" as const,
  };
}

Deno.test({
  name: "SidecarManager: start transitions Disabled -> Loading -> Running",
  // The manager intentionally keeps a watchExit task alive on the spawned
  // subprocess for restart-on-crash, so the default sanitizers complain
  // about leftover async ops. We assert clean stop below.
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    __resetForTesting();
    const mgr = sidecarManager();
    const port = freePort();
    const states: SidecarSnapshot["status"][] = [];
    mgr.subscribe((s) => {
      if (s.kind === "llama") states.push(s.status);
    });

    await mgr.start("llama", buildOpts(port));
    assertEquals(mgr.status("llama").status, "Running");
    assertEquals(states.at(-1), "Running");
    // First transition off "Disabled" must be "Loading".
    assertEquals(states[0], "Loading");

    await mgr.stop("llama");
    assertEquals(mgr.status("llama").status, "Disabled");
  },
});

Deno.test({
  name: "SidecarManager: second start supersedes the first and reaches Running",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    __resetForTesting();
    const mgr = sidecarManager();
    const port = freePort();

    await mgr.start("llama", buildOpts(port));
    assertEquals(mgr.status("llama").status, "Running");
    await mgr.start("llama", buildOpts(port));
    assertEquals(mgr.status("llama").status, "Running");
    await mgr.stop("llama");
  },
});

Deno.test({
  name: "SidecarManager: status() reports Disabled before any start",
  fn() {
    __resetForTesting();
    const mgr = sidecarManager();
    assertEquals(mgr.status("llama").status, "Disabled");
  },
});

Deno.test({
  name: "SidecarManager: a crash after stop() -> start() surfaces as Error, not swallowed",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    __resetForTesting();
    const mgr = sidecarManager();
    const port = freePort();
    const opts = buildOpts(port); // restartPolicy: "none" -> a crash emits Error and stops
    await mgr.start("llama", opts);
    assertEquals(mgr.status("llama").status, "Running");

    // An intentional stop sets pendingStop=true; the fresh start must clear it.
    // Before the fix, pendingStop lingered and the next crash was swallowed by
    // watchExit (treated as an intentional stop) with no Error and no restart.
    await mgr.stop("llama");
    await mgr.start("llama", opts);
    assertEquals(mgr.status("llama").status, "Running");

    // Crash the live process; this is an unexpected exit and must surface.
    await fetch(`http://127.0.0.1:${port}/exit`).catch(() => {});
    await waitForStatus(mgr, "Error", 8_000);
    assertEquals(mgr.status("llama").status, "Error");
  },
});

Deno.test({
  name: "SidecarManager: a crash during readiness aborts the probe instead of blocking start()",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    __resetForTesting();
    const mgr = sidecarManager();
    const port = freePort();
    const t0 = Date.now();
    // The process exits immediately, so the health endpoint never comes up.
    // watchExit must abort the readiness poll; without that, start() blocks
    // for the full startup window against a process that is already gone.
    await mgr.start("llama", {
      binary: Deno.execPath(),
      args: ["eval", "Deno.exit(1)"],
      readiness: { kind: "http" as const, url: `http://127.0.0.1:${port}/health` },
      startupTimeoutMs: 20_000,
      restartPolicy: "none" as const,
    });
    const elapsed = Date.now() - t0;
    assertEquals(mgr.status("llama").status, "Error");
    assert(elapsed < 10_000, `start() blocked ${elapsed}ms after the process exited`);
  },
});

Deno.test({
  name: "SidecarManager: an explicit start during crash-backoff supersedes the stale restart loop",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    __resetForTesting();
    const mgr = sidecarManager();
    const port = freePort();
    const opts = {
      ...buildOpts(port),
      restartPolicy: { maxAttempts: 3, initialDelayMs: 800, maxDelayMs: 800 },
    };
    await mgr.start("llama", opts);
    assertEquals(mgr.status("llama").status, "Running");

    // Crash: watchExit emits Error and arms the 800ms backoff loop.
    await fetch(`http://127.0.0.1:${port}/exit`).catch(() => {});
    await waitForStatus(mgr, "Error", 8_000);

    // Explicit start while the loop sleeps: it must win. The stale loop waking
    // up used to call start() with its old options, superseding (killing) the
    // freshly-started process and re-emitting Loading.
    await mgr.start("llama", buildOpts(port));
    assertEquals(mgr.status("llama").status, "Running");
    const states: SidecarSnapshot["status"][] = [];
    const unsub = mgr.subscribe((s) => {
      if (s.kind === "llama") states.push(s.status);
    });
    await new Promise((r) => setTimeout(r, 1_600)); // past the loop's wake-up
    unsub();
    assertEquals(states, [], "stale restart loop fired after an explicit start");
    assertEquals(mgr.status("llama").status, "Running");
    await mgr.stop("llama");
  },
});

async function waitForStatus(
  mgr: ReturnType<typeof sidecarManager>,
  status: SidecarSnapshot["status"],
  timeoutMs: number,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (mgr.status("llama").status === status) return;
    await new Promise((r) => setTimeout(r, 50));
  }
}
