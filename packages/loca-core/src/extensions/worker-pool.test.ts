// Smoke tests for the worker-pool singleton + configuration surface.
//
// Deep behavior (LRU eviction, ask-user pause/resume, idle TTL) requires
// driving real Deno.Command subprocesses against a fixture extension, which
// is heavy enough to live in tests/e2e/ rather than the fast unit pass.
// These tests cover the bits that can be exercised without spawning.

import { assertEquals } from "@std/assert";
import {
  __resetForTesting,
  DEFAULT_POOL_CONFIG,
  POOL_KEYS,
  poolConfigFrom,
  workerPool,
} from "./worker-pool.ts";
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

// The settings reach the pool at all.
//
// Before this wiring the three keys existed in the panel, carried real defaults,
// and were read by nothing in either core: `setConfig` had zero callers and the
// pool ran on its constants. The constants match the advertised defaults, so the
// panel told the truth right up until the user changed something.
Deno.test("poolConfigFrom: each tunable is read from the settings record", () => {
  const cfg = poolConfigFrom({
    "extensions.maxWarmWorkers": 3,
    "extensions.workerIdleMs": 45_000,
    "extensions.callTimeoutMs": 9_000,
  });
  assertEquals(cfg.maxWarmWorkers, 3);
  assertEquals(cfg.workerIdleMs, 45_000);
  assertEquals(cfg.callTimeoutMs, 9_000);
  // Not a setting, so it must NOT move with them.
  assertEquals(cfg.drainTimeoutMs, DEFAULT_POOL_CONFIG.drainTimeoutMs);
});

// Zero warm workers means no tool can ever run and a zero timeout fails every
// call instantly, so both are refused rather than applied. The panel has a
// `min`, but the record can be hand-edited, so the bound is re-asserted where
// it is read.
Deno.test("poolConfigFrom: absent and out-of-range values fall back", () => {
  assertEquals(poolConfigFrom({}), DEFAULT_POOL_CONFIG);
  assertEquals(
    poolConfigFrom({
      "extensions.maxWarmWorkers": 0,
      "extensions.workerIdleMs": 0,
      "extensions.callTimeoutMs": -1,
    }),
    DEFAULT_POOL_CONFIG,
  );
});

// D-139's failure, pre-empted: a watch set that omits a key its own reader uses
// means the user changes a setting and nothing happens.
Deno.test("POOL_KEYS covers every key poolConfigFrom reads", () => {
  for (const key of POOL_KEYS) {
    assertEquals(
      poolConfigFrom({ [key]: 7 }) === DEFAULT_POOL_CONFIG,
      false,
      `${key} is watched but not read`,
    );
  }
});
