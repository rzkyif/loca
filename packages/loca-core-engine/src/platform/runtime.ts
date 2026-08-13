// The engine's runtime handle: a module-level Host set once by the embedder
// before any engine service runs (core's main.ts at boot; the test harness in
// setup). Moved services reach the runtime through `host()` instead of importing
// a Deno singleton, so the ~250 call sites need no host threading and the same
// source runs under any Host. `attachHost` is idempotent (re-attaching the same
// or a fresh host just replaces it), which the per-test harness relies on.

import type { Host } from "../host.ts";

let current: Host | null = null;

/** Install the Host the engine runs on. Call once before using any engine
 *  service; safe to call again (replaces the handle) so a test harness can
 *  re-attach per run. */
export function attachHost(host: Host): void {
  current = host;
}

/** The attached Host. Throws if the embedder never called `attachHost`, which
 *  is always a wiring bug (a service ran before boot installed the host). */
export function host(): Host {
  if (!current) {
    throw new Error("engine host not attached: call attachHost(host) before using engine services");
  }
  return current;
}

/** Test-only: forget the attached host so a run can assert the unattached error
 *  or start from a clean slate. */
export function __detachHostForTesting(): void {
  current = null;
}
