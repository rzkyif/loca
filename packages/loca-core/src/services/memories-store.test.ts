// MemoriesStore: CRUD, exact-edit semantics, and rescan reconciliation
// (files are the source of truth). Mutating methods are async (host.fs) and
// serialized by a write lock so a read-modify-write can't interleave.

import { assertEquals, assertRejects } from "@std/assert";
import { join } from "@std/path";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { paths } from "../paths.ts";
import { AppError } from "@loca/core-engine";
import { memoriesStore, setMemoryProviderGate } from "@loca/core-engine/services/memories-store";

Deno.test("memories: create, get, list, title conflict", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    const doc = await store.create("knowledge", "Meeting Notes: Q3!", "# Notes\nhello");
    assertEquals(doc.filename, "meeting-notes-q3.md");
    assertEquals(doc.content, "# Notes\nhello");
    assertEquals(
      store.list().map((d) => d.title),
      ["Meeting Notes: Q3!"],
    );
    assertEquals((await store.getByTitle("Meeting Notes: Q3!"))?.id, doc.id);
    await assertRejects(
      () => store.create("knowledge", "Meeting Notes: Q3!", "again"),
      AppError,
      "already exists",
    );
    // Title conflict is case-insensitive: tools address memories by title
    // and could not tell "Notes" from "notes" apart.
    await assertRejects(
      () => store.create("knowledge", "meeting notes: q3!", "again"),
      AppError,
      "already exists",
    );
    assertEquals((await store.getByTitle("MEETING NOTES: Q3!"))?.id, doc.id);
    // Same slug, different title: filename gets a numeric suffix.
    const sibling = await store.create("knowledge", "Meeting Notes Q3", "x");
    assertEquals(sibling.filename, "meeting-notes-q3-2.md");
  } finally {
    await env.teardown();
  }
});

Deno.test("memories: skills are folders with SKILL.md, frontmatter, and bundled files", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    const body =
      "---\ndescription: Files a bug report\nsuggested-tools: [web_search, write_memory]\n---\n# Steps\ndo the thing";
    const skill = await store.create("skill", "File Bug", body);
    assertEquals(skill.kind, "skill");
    assertEquals(skill.filename, "file-bug");
    assertEquals(skill.content, body);
    // Frontmatter seeds the summary + suggested tools.
    assertEquals(skill.summary, "Files a bug report");
    assertEquals(skill.suggestedTools, ["web_search", "write_memory"]);
    // SKILL.md lives inside the folder; drop a bundled reference file beside it.
    const dir = join(paths().memoriesDir, skill.filename);
    Deno.writeTextFileSync(join(dir, "template.md"), "bug template");
    assertEquals((await store.get(skill.id)).files, ["template.md"]);
    assertEquals(await store.getFile(skill.id, "template.md"), "bug template");
    // Path traversal is rejected.
    await assertRejects(() => store.getFile(skill.id, "../secret"), AppError);
  } finally {
    await env.teardown();
  }
});

Deno.test("memories: writeFile and deleteFile manage a skill's bundled files", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    const skill = await store.create("skill", "Web Research", "# Steps\nsearch");
    // Create a bundled file, then read it back through the same store.
    await store.writeFile(skill.id, "checklist.md", "1. cross-check sources");
    assertEquals((await store.get(skill.id)).files, ["checklist.md"]);
    assertEquals(await store.getFile(skill.id, "checklist.md"), "1. cross-check sources");
    // Replace its content.
    await store.writeFile(skill.id, "checklist.md", "1. updated");
    assertEquals(await store.getFile(skill.id, "checklist.md"), "1. updated");
    // SKILL.md itself (any casing) and traversal are off-limits to the file API.
    await assertRejects(() => store.writeFile(skill.id, "SKILL.md", "x"), AppError);
    await assertRejects(() => store.writeFile(skill.id, "skill.md", "x"), AppError);
    await assertRejects(() => store.writeFile(skill.id, "../escape", "x"), AppError);
    await assertRejects(() => store.writeFile(skill.id, "sub/file.md", "x"), AppError);
    await assertRejects(() => store.writeFile(skill.id, "  ", "x"), AppError);
    // Delete removes it.
    await store.deleteFile(skill.id, "checklist.md");
    assertEquals((await store.get(skill.id)).files, []);
    await assertRejects(() => store.deleteFile(skill.id, "checklist.md"), AppError);
    // Knowledge has no bundled files.
    const note = await store.create("knowledge", "Note", "body");
    await assertRejects(() => store.writeFile(note.id, "x.md", "y"), AppError);
  } finally {
    await env.teardown();
  }
});

Deno.test("memories: summaryStale flips false once the summary pins the content", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    const doc = await store.create("knowledge", "Pinned", "body");
    // Fresh memory: never indexed, so stale.
    assertEquals((await store.get(doc.id)).summaryStale, true);
    // The indexer pins a summary to the current content hash.
    store.setSummary(doc.id, "a summary", doc.contentHash);
    assertEquals((await store.get(doc.id)).summaryStale, false);
    // Editing the content makes the pinned summary stale again.
    await store.replaceContent(doc.id, "new body");
    assertEquals((await store.get(doc.id)).summaryStale, true);
  } finally {
    await env.teardown();
  }
});

Deno.test("memories: an extension's memories are hidden unless the provider gate passes", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    // A user memory is always listed regardless of any gate.
    await store.create("knowledge", "Mine", "body");
    // Register a read-only memory shipped by extension "ext-a": base dir is the
    // parent of the install dir, so `${id}/${path}` resolves under the install dir.
    const baseDir = await Deno.makeTempDir();
    await Deno.mkdir(join(baseDir, "ext-a"), { recursive: true });
    Deno.writeTextFileSync(join(baseDir, "ext-a", "guide.md"), "# Guide");
    await store.registerExtensionMemories("ext-a", baseDir, [
      { kind: "knowledge", path: "guide.md" },
    ]);

    // No gate set: everything is visible (mobile / test default).
    assertEquals(
      store
        .list()
        .map((d) => d.title)
        .sort(),
      ["Guide", "Mine"],
    );

    // Gate says ext-a is NOT installed: its memory drops out, the row survives.
    setMemoryProviderGate((provider) => provider !== "ext-a");
    assertEquals(
      store.list().map((d) => d.title),
      ["Mine"],
    );

    // Gate says ext-a IS installed: it returns.
    setMemoryProviderGate(() => true);
    assertEquals(
      store
        .list()
        .map((d) => d.title)
        .sort(),
      ["Guide", "Mine"],
    );

    await Deno.remove(baseDir, { recursive: true });
  } finally {
    await env.teardown();
  }
});

Deno.test("memories: enable/disable toggles participation", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    const doc = await store.create("knowledge", "Toggle", "body");
    assertEquals(doc.enabled, true);
    assertEquals(store.setEnabled(doc.id, false).enabled, false);
    assertEquals((await store.get(doc.id)).enabled, false);
  } finally {
    await env.teardown();
  }
});

Deno.test("memories: editContent requires exactly one match", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    const doc = await store.create("knowledge", "Todo", "- alpha\n- beta\n- alpha tail");
    const { before, after, memory } = await store.editContent(doc.id, "- beta", "- gamma");
    assertEquals(before, "- alpha\n- beta\n- alpha tail");
    assertEquals(after, "- alpha\n- gamma\n- alpha tail");
    assertEquals(memory.content, after);
    await assertRejects(() => store.editContent(doc.id, "missing", "x"), AppError, "not found");
    await assertRejects(
      () => store.editContent(doc.id, "- alpha", "x"),
      AppError,
      "more than once",
    );
  } finally {
    await env.teardown();
  }
});

Deno.test("memories: a vanished file surfaces as not_found, not empty content", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    const doc = await store.create("knowledge", "Ghost", "body");
    // Delete the backing file out from under the row.
    Deno.removeSync(join(paths().memoriesDir, doc.filename));
    await assertRejects(() => store.get(doc.id), AppError, "missing on disk");
  } finally {
    await env.teardown();
  }
});

Deno.test("memories: rename keeps the file, delete removes it", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    const doc = await store.create("knowledge", "Old Title", "body");
    const renamed = store.rename(doc.id, "New Title");
    assertEquals(renamed.title, "New Title");
    assertEquals(renamed.filename, doc.filename);
    assertEquals((await store.get(doc.id)).content, "body");
    await store.delete(doc.id);
    assertEquals(store.list(), []);
    let fileGone = false;
    try {
      Deno.statSync(join(paths().memoriesDir, doc.filename));
    } catch {
      fileGone = true;
    }
    assertEquals(fileGone, true);
  } finally {
    await env.teardown();
  }
});

Deno.test("memories: rescan adds, removes, and flags changed files", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    const kept = await store.create("knowledge", "Kept", "kept body");
    const dropped = await store.create("knowledge", "Dropped", "dropped body");
    const dir = paths().memoriesDir;
    // Simulate external changes: a new file, a deleted file, an edited file.
    Deno.writeTextFileSync(join(dir, "handwritten.md"), "external content");
    Deno.removeSync(join(dir, dropped.filename));
    Deno.writeTextFileSync(join(dir, kept.filename), "kept body v2");

    const result = await store.rescan();
    assertEquals(result, { added: 1, removed: 1, changed: 1 });
    const titles = store.list().map((d) => d.title);
    assertEquals(titles.includes("handwritten"), true);
    assertEquals(titles.includes("Dropped"), false);
    assertEquals((await store.get(kept.id)).content, "kept body v2");
    // The changed hash invalidates the (hypothetical) summary pin.
    const state = store.listIndexStates().find((s) => s.id === kept.id)!;
    assertEquals(state.summarySourceHash === state.contentHash, false);
  } finally {
    await env.teardown();
  }
});
