// Memory context injection: the relevant-summaries prompt block (scored
// against stored embeddings with a caller-supplied query vector, so no
// embed sidecar is needed) and `@token` expansion in user messages.

import { assertEquals, assertStringIncludes } from "@std/assert";
import { join } from "@std/path";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { paths } from "../paths.ts";
import { memoriesStore } from "@loca/core-engine/services/memories-store";
import { embedSourceHash, memoryEmbedText } from "@loca/core-engine/services/relevance";
import {
  memoryTokenBlocks,
  relevantMemoriesPrompt,
} from "@loca/core-engine/services/memory-injection";

Deno.test("injection: ranks by similarity and applies the floor", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    const recipes = await store.create("knowledge", "Recipes", "pasta things");
    const recipesSummary = "Pasta recipes and cooking notes.";
    store.setSummary(recipes.id, recipesSummary, recipes.contentHash);
    // A summary memory must carry the hash the indexer would compute, or the
    // prompt-time freshness skip drops it as stale.
    store.setEmbedding(
      recipes.id,
      new Float32Array([1, 0]),
      embedSourceHash(memoryEmbedText("Recipes", recipesSummary)),
    );
    // "Taxes" has no summary, so it falls back to the dimension-only guard (its
    // hash is not checked) - exercising that path too.
    const taxes = await store.create("knowledge", "Taxes", "tax things");
    store.setEmbedding(taxes.id, new Float32Array([0, 1]), "h2");

    // Query aligned with "Recipes"; "Taxes" is orthogonal (below the floor).
    const block = await relevantMemoriesPrompt("dinner ideas", new Float32Array([1, 0]), 3, true);
    assertStringIncludes(block ?? "", "Recipes: Pasta recipes and cooking notes.");
    assertEquals(block?.includes("Taxes"), false);
    assertStringIncludes(block ?? "", "read_memory");

    // maxRelevant caps the list even when both clear the floor.
    const capped = await relevantMemoriesPrompt("anything", new Float32Array([0.8, 0.7]), 1, true);
    assertEquals((capped?.match(/^- /gm) ?? []).length, 1);
  } finally {
    await env.teardown();
  }
});

Deno.test("injection: skips a summary memory whose stored embedding hash is stale (embed-model switch)", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    // Fresh: hash matches the current model + summary, so it is scored.
    const fresh = await store.create("knowledge", "Fresh", "fresh body");
    store.setSummary(fresh.id, "Fresh summary.", fresh.contentHash);
    store.setEmbedding(
      fresh.id,
      new Float32Array([1, 0]),
      embedSourceHash(memoryEmbedText("Fresh", "Fresh summary.")),
    );
    // Stale: same dimension as the query (so it clears the length guard) but its
    // stored hash is from a different embed model, so the cosine would be
    // meaningless. It must be skipped, not scored, even though its vector would
    // otherwise rank identically to Fresh.
    const stale = await store.create("knowledge", "Stale", "stale body");
    store.setSummary(stale.id, "Stale summary.", stale.contentHash);
    store.setEmbedding(stale.id, new Float32Array([1, 0]), "hash-from-another-model");

    const block = await relevantMemoriesPrompt("anything", new Float32Array([1, 0]), 5, true);
    assertStringIncludes(block ?? "", "Fresh: Fresh summary.");
    assertEquals(block?.includes("Stale"), false);
  } finally {
    await env.teardown();
  }
});

Deno.test("injection: no embeddings or no query yields null", async () => {
  const env = await setupTestEnv();
  try {
    assertEquals(await relevantMemoriesPrompt("query", new Float32Array([1]), 3, true), null);
    await memoriesStore().create("knowledge", "Doc", "body");
    assertEquals(await relevantMemoriesPrompt(undefined, new Float32Array([1]), 3, true), null);
  } finally {
    await env.teardown();
  }
});

Deno.test("token expansion: matches filename stems, dedupes, ignores unknown", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    await store.create("knowledge", "Meeting Notes", "agenda body");
    const blocks = await memoryTokenBlocks(
      "see @meeting-notes and again @meeting-notes plus @unknown-token",
    );
    assertStringIncludes(blocks ?? "", '[Knowledge @meeting-notes "Meeting Notes"');
    assertStringIncludes(blocks ?? "", "agenda body");
    // Injected content is fenced + flagged as untrusted data (prompt-injection guard).
    assertStringIncludes(blocks ?? "", "--- BEGIN KNOWLEDGE ---");
    assertEquals((blocks?.match(/\[Knowledge /g) ?? []).length, 1);
    assertEquals(blocks?.includes("unknown-token"), false);

    // Email-like text must not trigger (token must not follow a word char).
    assertEquals(await memoryTokenBlocks("mail me at notes@meeting-notes.com"), null);
    assertEquals(await memoryTokenBlocks("no tokens here"), null);
  } finally {
    await env.teardown();
  }
});

Deno.test("token expansion: a quoted token references a filename with spaces", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    // A hand-placed file whose stem has a space: only the quoted form can
    // address it (created-in-app memories get slug filenames instead).
    Deno.writeTextFileSync(join(paths().memoriesDir, "My Notes.md"), "spaced body");
    await store.rescan();

    const blocks = await memoryTokenBlocks('look at @"my notes" please');
    assertStringIncludes(blocks ?? "", "spaced body");
    assertStringIncludes(blocks ?? "", '[Knowledge @"my notes"');

    // The bare form can't match a spaced stem.
    assertEquals(await memoryTokenBlocks("look at @my please"), null);
  } finally {
    await env.teardown();
  }
});

Deno.test("token expansion: a bare token may contain dots, ignoring trailing punctuation", async () => {
  const env = await setupTestEnv();
  try {
    const store = memoriesStore();
    // A stem with no spaces but characters outside the slug set (here a dot) is
    // addressable bare: quotes are only needed to span whitespace.
    Deno.writeTextFileSync(join(paths().memoriesDir, "node.js.md"), "dotted body");
    await store.rescan();

    // The sentence-ending period must not be swallowed into the stem.
    const blocks = await memoryTokenBlocks("see @node.js.");
    assertStringIncludes(blocks ?? "", "dotted body");
    assertStringIncludes(blocks ?? "", '[Knowledge @node.js "node.js"');
  } finally {
    await env.teardown();
  }
});
