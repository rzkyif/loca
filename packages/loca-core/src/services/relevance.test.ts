// topKBySimilarity ordering + bounds. cosineNormalized itself is exercised
// through these (and through tool-filter.test.ts, which ranks via the same
// helper).

import { assert, assertEquals, assertFalse, assertThrows } from "@std/assert";
import {
  cosineNormalized,
  toolEmbedText,
  toolNameMentioned,
  topKBySimilarity,
} from "@loca/core-engine/services/relevance";

function vec(...xs: number[]): Float32Array {
  return new Float32Array(xs);
}

Deno.test("cosineNormalized: dot product of equal-length vectors", () => {
  assertEquals(cosineNormalized(vec(1, 0), vec(1, 0)), 1);
  assertEquals(cosineNormalized(vec(1, 0), vec(0, 1)), 0);
});

Deno.test("cosineNormalized: throws on a length mismatch (stale index) instead of truncating", () => {
  assertThrows(() => cosineNormalized(vec(1, 1, 1), vec(1)), Error, "length mismatch");
});

Deno.test("topKBySimilarity: ranks by descending cosine and caps at topK", () => {
  const query = vec(1, 0);
  const items = [
    { item: "orthogonal", vector: vec(0, 1) },
    { item: "exact", vector: vec(1, 0) },
    { item: "close", vector: vec(0.9, 0.1) },
  ];
  const top2 = topKBySimilarity(query, items, 2);
  assertEquals(
    top2.map((s) => s.item),
    ["exact", "close"],
  );
  assertEquals(top2[0].score, 1);

  assertEquals(topKBySimilarity(query, items, 10).length, 3);
  assertEquals(topKBySimilarity(query, items, 0), []);
  assertEquals(topKBySimilarity(query, [], 5), []);
});

Deno.test("toolEmbedText: leads with the humanized name, then description and triggers", () => {
  const text = toolEmbedText({
    name: "web_search",
    description: "Search the web.",
    triggers: ["look this up online"],
  });
  assertEquals(text, "web search\nSearch the web.\nlook this up online");
  // Missing triggers default to no trigger lines (a trailing newline remains).
  assertEquals(
    toolEmbedText({ name: "get_datetime", description: "The time." }),
    "get datetime\nThe time.\n",
  );
});

Deno.test("toolNameMentioned: matches the humanized or literal name as whole words", () => {
  assert(toolNameMentioned("run web search please", "web_search"));
  assert(toolNameMentioned("use web_search now", "web_search"));
  // The reported case: naming a snake_case tool with the underscore form.
  assert(toolNameMentioned("Use the write_memory tool", "write_memory"));
  assert(toolNameMentioned("open the Calculator tool", "calculator"));
  // Contiguous + word-bounded: a name whose words are split by other words
  // does not match, so an incidental phrase can't force an unrelated tool in.
  assertFalse(toolNameMentioned("open the app store", "open_app"));
  assertFalse(toolNameMentioned("search my inbox", "web_search"));
  // Trivially short names are ignored so they can't match noise.
  assertFalse(toolNameMentioned("go to the shop", "go"));
});
