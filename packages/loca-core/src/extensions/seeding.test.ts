// Seed-marker logic for the extensions Loca ships. The branches here never touch
// the network (marker-already-set, already-installed, and devOnly-skipped); the
// actual CDN/codebase install path is exercised by the installer tests + manual
// verification. Tests run on the default "stable" channel, so the dev-only samples
// extension is skipped and only the built-in is processed.

import { assertEquals, assertExists } from "@std/assert";
import { BUILTIN_EXTENSION_ID, SAMPLES_EXTENSION_ID } from "@loca/shared";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { seedExtensionsIfNeeded } from "./seeding.ts";
import { extensionsRegistry } from "./registry.ts";
import { paths } from "../paths.ts";
import type { InstallEventSink } from "./installer.ts";

const NOOP_SINK: InstallEventSink = {
  log() {},
  done() {},
};

async function markerExists(id: string): Promise<boolean> {
  try {
    await Deno.stat(paths().seededMarkerFile(id));
    return true;
  } catch {
    return false;
  }
}

Deno.test("seedExtensionsIfNeeded: no-ops when the built-in marker is already set", async () => {
  const env = await setupTestEnv();
  try {
    await Deno.writeTextFile(paths().seededMarkerFile(BUILTIN_EXTENSION_ID), "");
    await seedExtensionsIfNeeded(NOOP_SINK);
    // Respects a prior delete: nothing is (re)installed and the marker stays set.
    assertEquals(extensionsRegistry().list().length, 0);
    assertEquals(await markerExists(BUILTIN_EXTENSION_ID), true);
  } finally {
    await env.teardown();
  }
});

Deno.test("seedExtensionsIfNeeded: adopts an existing built-in and marks seeded", async () => {
  const env = await setupTestEnv();
  try {
    extensionsRegistry().upsertExtension({
      id: BUILTIN_EXTENSION_ID,
      source: "seeded",
      displayName: "Loca built-in extension",
      version: "1.0.0",
      installedPath: "/tmp/loca-extension-builtin",
      manifestHash: "h",
      contentHash: "c",
    });
    await seedExtensionsIfNeeded(NOOP_SINK);
    // An already-present built-in just records the seed marker (no re-download).
    assertEquals(await markerExists(BUILTIN_EXTENSION_ID), true);
    assertExists(extensionsRegistry().get(BUILTIN_EXTENSION_ID));
  } finally {
    await env.teardown();
  }
});

Deno.test("seedExtensionsIfNeeded: skips the dev-only samples extension outside dev", async () => {
  const env = await setupTestEnv();
  try {
    // Built-in marker set so its branch no-ops; samples is devOnly and the test
    // channel is "stable", so it must never be seeded or marked.
    await Deno.writeTextFile(paths().seededMarkerFile(BUILTIN_EXTENSION_ID), "");
    await seedExtensionsIfNeeded(NOOP_SINK);
    assertEquals(extensionsRegistry().get(SAMPLES_EXTENSION_ID), undefined);
    assertEquals(await markerExists(SAMPLES_EXTENSION_ID), false);
  } finally {
    await env.teardown();
  }
});
