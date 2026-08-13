// core-settings sparse persistence: load + patch + atomic write +
// listener fan-out. File-backed via tempdir.

import { assertEquals } from "@std/assert";
import {
  loadCoreSettings,
  loadEffective,
  patchClientSettings,
  patchCoreSettings,
  resetAllClientSettings,
  subscribeClientSettings,
  subscribeCoreSettings,
} from "@loca/core-engine/services/core-settings";
import { createTestClient, setupTestEnv } from "../../tests/helpers/db.ts";
import { paths } from "../paths.ts";

Deno.test("loadCoreSettings: returns {} when settings.json does not exist", async () => {
  const env = await setupTestEnv();
  try {
    assertEquals(await loadCoreSettings(), {});
  } finally {
    await env.teardown();
  }
});

Deno.test("patchCoreSettings: adds and persists the key", async () => {
  const env = await setupTestEnv();
  try {
    const merged = await patchCoreSettings({ theme: "dark" });
    assertEquals(merged.theme, "dark");
    const onDisk = JSON.parse(await Deno.readTextFile(paths().settingsFile));
    assertEquals(onDisk.theme, "dark");
  } finally {
    await env.teardown();
  }
});

Deno.test("patchCoreSettings: null/undefined deletes the key (sparse storage)", async () => {
  const env = await setupTestEnv();
  try {
    await patchCoreSettings({ k1: "v1", k2: "v2" });
    const after = await patchCoreSettings({ k1: null });
    assertEquals("k1" in after, false);
    assertEquals(after.k2, "v2");
  } finally {
    await env.teardown();
  }
});

Deno.test("patchCoreSettings: same value is a no-op (not in changedKeys)", async () => {
  const env = await setupTestEnv();
  try {
    await patchCoreSettings({ k: "v" });
    let fired = 0;
    let lastChangedSize = -1;
    const unsubscribe = subscribeCoreSettings((_s, changed) => {
      fired++;
      lastChangedSize = changed.size;
    });
    try {
      await patchCoreSettings({ k: "v" });
      // No change, no listener fire.
      assertEquals(fired, 0);
      await patchCoreSettings({ k: "v2" });
      assertEquals(fired, 1);
      assertEquals(lastChangedSize, 1);
    } finally {
      unsubscribe();
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("subscribeCoreSettings: unsubscribe stops the listener", async () => {
  const env = await setupTestEnv();
  try {
    let fired = 0;
    const unsubscribe = subscribeCoreSettings(() => {
      fired++;
    });
    await patchCoreSettings({ k: "v" });
    assertEquals(fired, 1);
    unsubscribe();
    await patchCoreSettings({ k: "v2" });
    assertEquals(fired, 1);
  } finally {
    await env.teardown();
  }
});

Deno.test("loadEffective: core base overlaid with a client's overrides (overlay wins)", async () => {
  const env = await setupTestEnv();
  try {
    const client = createTestClient("a");
    // Shared core value plus a per-client override of a different key.
    await patchCoreSettings({ "llm.temperature": 0.5, "prompts.defaultSystemPrompt": "shared" });
    await patchClientSettings(client, { "prompts.defaultSystemPrompt": "mine" });
    const eff = await loadEffective(client);
    assertEquals(eff["llm.temperature"], 0.5); // inherited from core
    assertEquals(eff["prompts.defaultSystemPrompt"], "mine"); // overlay wins
    // The shared core store is untouched by the per-client patch.
    assertEquals((await loadCoreSettings())["prompts.defaultSystemPrompt"], "shared");
  } finally {
    await env.teardown();
  }
});

Deno.test("patchClientSettings: per-client isolation + sparse delete", async () => {
  const env = await setupTestEnv();
  try {
    const a = createTestClient("a");
    const b = createTestClient("b");
    const res = await patchClientSettings(a, { "llm.temperature": 0.9 });
    assertEquals(res.values["llm.temperature"], 0.9);
    // b never sees a's value.
    assertEquals("llm.temperature" in (await loadEffective(b)), false);
    assertEquals((await loadEffective(a))["llm.temperature"], 0.9);
    // null deletes a's override (reverts to the core/default value).
    const del = await patchClientSettings(a, { "llm.temperature": null });
    assertEquals(del.deleted, ["llm.temperature"]);
    assertEquals("llm.temperature" in (await loadEffective(a)), false);
  } finally {
    await env.teardown();
  }
});

Deno.test("resetAllClientSettings: wipes every overlay and notifies each affected client", async () => {
  const env = await setupTestEnv();
  try {
    const a = createTestClient("a");
    const b = createTestClient("b");
    await patchClientSettings(a, { "llm.temperature": 0.9, "prompts.defaultSystemPrompt": "mine" });
    await patchClientSettings(b, { "llm.topP": 0.8 });
    const fired = new Map<string, string[]>();
    const unsub = subscribeClientSettings((clientId, _values, deleted) => {
      fired.set(clientId, deleted);
    });
    try {
      await resetAllClientSettings();
      // Both overlays are gone (effective view falls back to core/defaults).
      assertEquals("llm.temperature" in (await loadEffective(a)), false);
      assertEquals("prompts.defaultSystemPrompt" in (await loadEffective(a)), false);
      assertEquals("llm.topP" in (await loadEffective(b)), false);
      // Each affected client was notified with its just-deleted keys so a
      // connected client re-baselines without reconnecting.
      assertEquals(
        new Set(fired.get(a)),
        new Set(["llm.temperature", "prompts.defaultSystemPrompt"]),
      );
      assertEquals(fired.get(b), ["llm.topP"]);
    } finally {
      unsub();
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("subscribeClientSettings: fires with the owning client id and delta", async () => {
  const env = await setupTestEnv();
  try {
    const a = createTestClient("a");
    let firedClient: string | null = null;
    let firedValues: Record<string, unknown> = {};
    const unsub = subscribeClientSettings((clientId, values) => {
      firedClient = clientId;
      firedValues = values;
    });
    try {
      await patchClientSettings(a, { "llm.topP": 0.8 });
      assertEquals(firedClient, a);
      assertEquals(firedValues["llm.topP"], 0.8);
    } finally {
      unsub();
    }
  } finally {
    await env.teardown();
  }
});
