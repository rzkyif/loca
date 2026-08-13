// Smoke tests for the worker-pool singleton + configuration surface.
//
// Deep behavior (LRU eviction, ask-user pause/resume, idle TTL) requires
// driving real Deno.Command subprocesses against a fixture extension, which
// is heavy enough to live in tests/e2e/ rather than the fast unit pass.
// These tests cover the bits that can be exercised without spawning.

import { assertEquals } from "@std/assert";
import { __resetForTesting, DEFAULT_POOL_CONFIG, workerPool } from "./worker-pool.ts";
import { setupTestEnv } from "../../tests/helpers/db.ts";

Deno.test("DEFAULT_POOL_CONFIG: sane defaults", () => {
  // The defaults are documented contract for `startCall`'s timeout
  // budget and the warm-worker cap. Catch accidental zero/negative
  // changes that would silently break call ordering.
  assertEquals(DEFAULT_POOL_CONFIG.callTimeoutMs > 0, true);
  assertEquals(DEFAULT_POOL_CONFIG.workerIdleMs > 0, true);
  assertEquals(DEFAULT_POOL_CONFIG.drainTimeoutMs > 0, true);
  assertEquals(DEFAULT_POOL_CONFIG.maxWarmWorkers >= 1, true);
});

Deno.test("workerPool(): returns a stable singleton across calls", async () => {
  const env = await setupTestEnv();
  try {
    const a = workerPool();
    const b = workerPool();
    assertEquals(a === b, true);
  } finally {
    await env.teardown();
  }
});

Deno.test("__resetForTesting: drops the singleton so the next call rebuilds", async () => {
  const env = await setupTestEnv();
  try {
    const first = workerPool();
    __resetForTesting();
    const second = workerPool();
    assertEquals(first === second, false);
  } finally {
    await env.teardown();
  }
});
