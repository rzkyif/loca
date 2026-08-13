import { assert, assertEquals, assertRejects } from "@std/assert";
import { dirname, join } from "@std/path";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { paths } from "../paths.ts";
import { buildStorageTree, clearStorageCategory, deleteStoragePaths } from "./storage.ts";
import { patchCoreSettings } from "@loca/core-engine/services/core-settings";
import { setSecret } from "@loca/core-engine/services/secrets";
import type { StorageCategory, StorageNode } from "@loca/shared";
import { AppError } from "@loca/core-engine";

async function writeModel(rel: string, bytes: string): Promise<string> {
  const abs = join(paths().modelsDir, rel);
  await Deno.mkdir(dirname(abs), { recursive: true });
  await Deno.writeTextFile(abs, bytes);
  return abs;
}

function category(tree: { categories: StorageCategory[] }, id: string): StorageCategory {
  const c = tree.categories.find((c) => c.id === id);
  assert(c, `category ${id} missing`);
  return c;
}

// Keep requirement recomputation (fired by delete/clear) offline.
function stubFetch(): () => void {
  const orig = globalThis.fetch;
  globalThis.fetch = () => Promise.reject(new Error("offline test"));
  return () => {
    globalThis.fetch = orig;
  };
}

Deno.test("buildStorageTree: groups models, exposes every category, total = sum of sizes", async () => {
  const env = await setupTestEnv();
  try {
    await writeModel("unsloth/Qwen3/model.gguf", "A".repeat(10));
    await writeModel("unsloth/Qwen3/mmproj-F16.gguf", "B".repeat(20));
    await writeModel("ggml-org/whisper/ggml-base.bin", "C".repeat(5));
    await writeModel("unsloth/Qwen3/README.md", "ignore-me"); // non-model file

    const tree = await buildStorageTree();
    const models = category(tree, "models");
    const byName = new Map(models.nodes.map((n) => [n.name, n]));

    const folder = byName.get("unsloth/Qwen3");
    assert(folder && folder.kind === "folder");
    assertEquals(folder.children.length, 2); // gguf + mmproj; README excluded
    assertEquals(folder.size, 30);
    assert(
      [...byName.keys()].some((k) => k.startsWith("ggml-org/whisper/")),
      "single-file repo should be inlined",
    );
    assertEquals(models.size, 35);

    // Every category id is present.
    for (const id of [
      "models",
      "binaries",
      "cache",
      "logs",
      "sessions",
      "extensions",
      "settings",
    ]) {
      category(tree, id);
    }
    assertEquals(
      tree.total_size,
      tree.categories.reduce((s, c) => s + c.size, 0),
    );
    assertEquals(join(tree.root_path, "models"), paths().modelsDir);
  } finally {
    await env.teardown();
  }
});

Deno.test("deleteStoragePaths: removes a model file; unknown/out-of-tree paths are skipped", async () => {
  const env = await setupTestEnv();
  const restore = stubFetch();
  try {
    const abs = await writeModel("u/r/model.gguf", "X".repeat(8));
    // A path that isn't a node in the tree is a no-op (not an error).
    await deleteStoragePaths([join(paths().modelsDir, "..", "escape.txt")]);
    await Deno.stat(abs); // still there
    await deleteStoragePaths([abs]);
    await assertRejects(() => Deno.stat(abs), Deno.errors.NotFound);
  } finally {
    restore();
    await env.teardown();
  }
});

Deno.test("deleteStoragePaths: refuses a model required by current settings", async () => {
  const env = await setupTestEnv();
  const restore = stubFetch();
  try {
    const abs = await writeModel("u/r/model.gguf", "X".repeat(8));
    await patchCoreSettings({
      "llm.provider": "local",
      "llm.modelPath": "@u/r/main/model.gguf",
    });

    const tree = await buildStorageTree();
    const node = category(tree, "models").nodes.find((n: StorageNode) => n.path === abs);
    assert(node?.lock_reason, "required model should be locked");

    await assertRejects(() => deleteStoragePaths([abs]), AppError, "in-use");
    await Deno.stat(abs); // not deleted
  } finally {
    restore();
    await env.teardown();
  }
});

Deno.test("clearStorageCategory('settings'): resets settings and wipes secrets", async () => {
  const env = await setupTestEnv();
  const restore = stubFetch();
  try {
    await patchCoreSettings({ "llm.modelPath": "@u/r/main/x.gguf" });
    await setSecret("OPENAI_API_KEY", "sk-test");
    assert((await Deno.stat(paths().secretsEncFile)).isFile);

    await clearStorageCategory("settings");

    // settings.json is now empty (sparse = all defaults).
    assertEquals(JSON.parse(await Deno.readTextFile(paths().settingsFile)), {});
    await assertRejects(() => Deno.stat(paths().secretsEncFile), Deno.errors.NotFound);
  } finally {
    restore();
    await env.teardown();
  }
});
