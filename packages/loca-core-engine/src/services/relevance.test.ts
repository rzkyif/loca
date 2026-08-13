// Similarity primitives shared by tool- and memory-relevance. cosineNormalized
// throws on a dimension mismatch (a stale index across an embed-model switch),
// which is intentional as a loud signal; callers MUST pre-filter incomparable
// vectors before scoring so one stale vector can't abort the whole batch. These
// tests pin both the throw and the filter-then-score contract the memory- and
// tool-relevance paths depend on.

import { assert, assertEquals, assertThrows } from "@std/assert";
import { cosineNormalized, topKBySimilarity } from "./relevance.ts";

function unit(vals: number[]): Float32Array {
  const v = new Float32Array(vals);
  let norm = 0;
  for (const x of v) norm += x * x;
  norm = Math.sqrt(norm) || 1;
  for (let i = 0; i < v.length; i++) v[i] /= norm;
  return v;
}

Deno.test("cosineNormalized: throws on a dimension mismatch (stale-index signal)", () => {
  assertThrows(
    () => cosineNormalized(new Float32Array(1536), new Float32Array(384)),
    Error,
    "vector length mismatch",
  );
});

Deno.test("topKBySimilarity: one incomparable vector aborts the whole batch (why callers must pre-filter)", () => {
  const query = unit([1, 0, 0]);
  const items = [
    { item: "a", vector: unit([1, 0, 0]) },
    { item: "stale", vector: new Float32Array(2) }, // different dimension
    { item: "b", vector: unit([0, 1, 0]) },
  ];
  // Unfiltered, a single stale-dimension vector throws and drops all results -
  // this is exactly the memory-injection crash the dimension pre-filter fixes.
  assertThrows(() => topKBySimilarity(query, items, 5), Error, "vector length mismatch");
});

Deno.test("topKBySimilarity: pre-filtering by dimension scores the fresh vectors and skips the stale one", () => {
  const query = unit([1, 0, 0]);
  const items = [
    { item: "a", vector: unit([1, 0, 0]) },
    { item: "stale", vector: new Float32Array(2) },
    { item: "b", vector: unit([0, 1, 0]) },
  ];
  // The fix (memory-injection relevantMemories): keep only same-dimension vectors
  // before scoring, so the stale one reads as "not embedded" instead of throwing.
  const filtered = items.filter((i) => i.vector.length === query.length);
  const scored = topKBySimilarity(query, filtered, 5);
  assertEquals(scored.length, 2);
  assertEquals(scored[0].item, "a"); // exact match ranks first
  assert(scored.every((s) => s.item !== "stale"));
});
