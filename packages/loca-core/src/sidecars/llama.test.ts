// llamaReadinessTimeoutMs: the llama-server readiness window scales with model
// size so a large GGUF on slow hardware isn't false-timed-out into the flap
// guard. Pure function, no I/O.
//
// llamaStartArgsFromSettings: the supportImages fallback must match the schema
// default (false). The requirements flow decides whether to download the mmproj
// from the same key, so a mismatched fallback here passes --mmproj for a file
// that was never fetched and llama-server exits on load.

import { assert, assertEquals } from "@std/assert";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { llamaReadinessTimeoutMs, llamaStartArgsFromSettings } from "./llama.ts";

const GiB = 1_073_741_824;

Deno.test("llamaReadinessTimeoutMs: a tiny model still gets the 60s base", () => {
  assertEquals(llamaReadinessTimeoutMs(0), 60_000);
  assertEquals(llamaReadinessTimeoutMs(0.5 * GiB), 60_000 + Math.ceil(0.5 * 25_000));
});

Deno.test("llamaReadinessTimeoutMs: scales ~25s per GiB above the base", () => {
  assertEquals(llamaReadinessTimeoutMs(2 * GiB), 60_000 + 50_000);
  assertEquals(llamaReadinessTimeoutMs(9 * GiB), 60_000 + 225_000);
});

Deno.test("llamaReadinessTimeoutMs: caps so a pathological size can't wait forever", () => {
  assertEquals(llamaReadinessTimeoutMs(100 * GiB), 600_000);
});

Deno.test("llamaStartArgsFromSettings: absent supportImages follows the schema default (mmproj)", async () => {
  const env = await setupTestEnv();
  try {
    const on = llamaStartArgsFromSettings({});
    assert(on?.mmprojPath?.endsWith("mmproj-F16.gguf"), "the default resolves the mmproj path");
    const off = llamaStartArgsFromSettings({ "llm.supportImages": false });
    assertEquals(off?.mmprojPath, undefined);
  } finally {
    await env.teardown();
  }
});
