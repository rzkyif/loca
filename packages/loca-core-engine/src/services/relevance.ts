// Embedding-similarity primitives shared by tool relevance filtering and
// memory relevance injection. One implementation so the two stay
// rank-identical: cosine over L2-normalized vectors, top-K selection, and
// the embed-plus-source-hash pairing the reindex paths persist.

import { activeEmbedModelId, embed } from "./embedding.ts";
import { sha256HexSync } from "../platform/hash.ts";

// Both inputs are assumed L2-normalized (the embedding sidecar + embedding.ts
// return unit vectors). Cosine reduces to dot product in that case.
export function cosineNormalized(a: Float32Array, b: Float32Array): number {
  // A length mismatch means vectors from different embedding models (a stale
  // index) got compared. Silently truncating to the shorter one would return a
  // plausible-but-meaningless score, so fail loudly instead.
  if (a.length !== b.length) {
    throw new Error(`cosine: vector length mismatch (${a.length} vs ${b.length}); reindex needed`);
  }
  let sum = 0;
  for (let i = 0; i < a.length; i++) sum += a[i] * b[i];
  return sum;
}

/** Score every item against the query vector and return the top-K by
 *  descending cosine. Stable for ties (sort is stable in V8). */
export function topKBySimilarity<T>(
  queryVector: Float32Array,
  items: Iterable<{ item: T; vector: Float32Array }>,
  topK: number,
): Array<{ item: T; score: number }> {
  const scored: Array<{ item: T; score: number }> = [];
  for (const { item, vector } of items) {
    scored.push({ item, score: cosineNormalized(queryVector, vector) });
  }
  scored.sort((a, b) => b.score - a.score);
  return scored.slice(0, Math.max(0, topK));
}

/** The staleness hash for an embedded text. The embedding model identity is
 *  folded in so vectors from a different model (never comparable) read as
 *  stale and re-embed instead of silently mis-scoring. Every freshness
 *  comparison against a stored sourceHash must use this, not a raw sha256. */
export function embedSourceHash(text: string): string {
  return sha256HexSync(`${activeEmbedModelId()}\n${text}`);
}

/** The text embedded for a tool's relevance vector. Defined once so the reindex
 *  path and the phase-1 freshness check derive the same string (and therefore
 *  the same source hash). The (humanized) tool name leads so a query that names
 *  the tool ranks it up, followed by the description and the example triggers. */
export function toolEmbedText(t: {
  name: string;
  description: string;
  triggers?: readonly string[];
}): string {
  return `${humanizeName(t.name)}\n${t.description}\n${(t.triggers ?? []).join("\n")}`;
}

/** The text embedded for a memory's relevance vector. Defined once (like
 *  `toolEmbedText`) so the background indexer and the prompt-time freshness
 *  check derive the same string, and therefore the same source hash. The
 *  indexer passes the memory's summary, or a head slice of its content until a
 *  summary exists; the prompt-time skip passes the summary it already has in
 *  the memory meta. */
export function memoryEmbedText(title: string, summaryOrContentHead: string): string {
  return `${title}\n${summaryOrContentHead}`;
}

/** True when the user's message directly names a tool: the tool's name, spelled
 *  literally (`web_search`) or humanized (`web search`), appears as a run of
 *  whole words in the query. The match is contiguous and word-bounded, so
 *  "open the app store" does NOT match `open_app` but "run web search" matches
 *  `web_search`. The caller force-includes a directly-named tool regardless of
 *  its embedding rank, so a mentioned tool always reaches the model. */
export function toolNameMentioned(query: string, name: string): boolean {
  const needle = normalizeWords(name);
  // Guard against a trivially short name (e.g. a 1-2 char tool) matching noise.
  if (needle.replaceAll(" ", "").length < 3) return false;
  return ` ${normalizeWords(query)} `.includes(` ${needle} `);
}

/** Turn a snake/kebab tool name into space-separated words ("web_search" ->
 *  "web search") for embedding and matching. */
function humanizeName(name: string): string {
  return name.replace(/[_-]+/g, " ").trim();
}

/** Lowercase, collapse every non-alphanumeric run to a single space, and trim,
 *  so a phrase can be tested as a contiguous whole-word substring. */
function normalizeWords(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

/** Embed one text and pair the vector with its staleness hash, so an index
 *  row can later detect drift by hash comparison. Returns null when the
 *  embed sidecar yields nothing. Callers should gate on
 *  isEmbeddingModelReady() to avoid opaque worker errors. */
export async function embedWithHash(
  text: string,
): Promise<{ vector: Float32Array; sourceHash: string } | null> {
  const [vector] = await embed([text]);
  if (!vector) return null;
  return { vector, sourceHash: embedSourceHash(text) };
}
