// Emits `packages/loca-core-rs/src/memories/testdata/relevance_vectors.json`:
// the similarity oracle for D5, taken from the frozen TypeScript rather than
// from a reading of it.
//
//   deno run -A scripts/parity/relevance-vectors.ts
//
// Why bit patterns rather than decimal numbers. The defect this fixture exists
// to catch is arithmetic done in `f32` where JavaScript does it in `f64`: the
// scores stay plausible and the ranking changes. An approximate comparison
// passes on the broken implementation, so the fixture records every float as
// its IEEE-754 bit pattern (`f32` as u32 hex, `f64` as u64 hex) and the Rust
// test compares bits. JSON's own decimal round-trip is then irrelevant.
//
// Four of the five sections call the real exported functions. The fifth,
// `l2normalize`, cannot: it is private to `services/embedding.ts`
// (`embedding.ts:165-173`) and is reachable only through `embed()`, which needs
// a configured host, settings and a live endpoint. Its four lines are
// transcribed here under `normalizeLikeEmbedding`, and the transcription is
// disclosed rather than hidden because it is the one place this fixture is not
// an oracle. What makes it trustworthy is the discrimination check below.
//
// THE DISCRIMINATION CHECK. A fixture that both the correct and the broken
// implementation satisfy proves nothing, and there is no way to tell by reading
// it. So this emitter also computes what each mutation WOULD produce (`f32`
// accumulation for cosine, a single `f32` divide for normalisation) and refuses
// to write the file unless enough cases actually differ. The fixture therefore
// carries a proof that it can fail, and `assertDiscriminates` is what a reviewer
// should look at first.

import {
  cosineNormalized,
  embedSourceHash,
  memoryEmbedText,
  topKBySimilarity,
} from "../../packages/loca-core-engine/src/services/relevance.ts";
import { activeEmbedModelId } from "../../packages/loca-core-engine/src/services/embedding.ts";
import { sha256HexSync } from "../../packages/loca-core-engine/src/platform/hash.ts";
import { EMBED_REPO } from "../../packages/loca-shared/src/domain/settings/model-files.ts";

const OUT = new URL(
  "../../packages/loca-core-rs/src/memories/testdata/relevance_vectors.json",
  import.meta.url,
);

// --- bit-pattern helpers -----------------------------------------------------

const f32buf = new ArrayBuffer(4);
const f32view = new Float32Array(f32buf);
const u32view = new Uint32Array(f32buf);
const f64buf = new ArrayBuffer(8);
const f64view = new Float64Array(f64buf);
const u64view = new BigUint64Array(f64buf);

/** One f32 lane as `0x`-prefixed 8-hex-digit bits. */
function f32bits(v: number): string {
  f32view[0] = v;
  return `0x${u32view[0].toString(16).padStart(8, "0")}`;
}

/** One f64 as `0x`-prefixed 16-hex-digit bits. */
function f64bits(v: number): string {
  f64view[0] = v;
  return `0x${u64view[0].toString(16).padStart(16, "0")}`;
}

function bitsOf(v: Float32Array): string[] {
  return [...v].map(f32bits);
}

// --- deterministic inputs ----------------------------------------------------

// xorshift32, so the fixture regenerates byte-identically on any machine.
function rng(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s ^= s << 13;
    s >>>= 0;
    s ^= s >>> 17;
    s ^= s << 5;
    s >>>= 0;
    return s / 0x1_0000_0000;
  };
}

/** Values in [-1, 1), rounded to f32 exactly as a real vector would be. */
function randomVector(width: number, seed: number): Float32Array {
  const next = rng(seed);
  const v = new Float32Array(width);
  for (let i = 0; i < width; i++) v[i] = next() * 2 - 1;
  return v;
}

/** A vector whose terms nearly cancel: half the lanes are the negation of the
 *  other half, scaled, so the running sum revisits zero repeatedly. Catastrophic
 *  cancellation is where an f32 accumulator loses the most, which is exactly the
 *  regime the fixture needs to cover. */
function cancellingVector(width: number, seed: number): Float32Array {
  const next = rng(seed);
  const v = new Float32Array(width);
  for (let i = 0; i < width; i += 2) {
    const x = next() * 2 - 1;
    v[i] = x;
    if (i + 1 < width) v[i + 1] = -x * 0.999_999;
  }
  return v;
}

/** Denormal and near-denormal f32 lanes, where a product underflows to zero in
 *  f32 and does not in f64. */
function denormalVector(width: number, seed: number): Float32Array {
  const next = rng(seed);
  const v = new Float32Array(width);
  for (let i = 0; i < width; i++) {
    v[i] = (next() * 2 - 1) * (i % 3 === 0 ? 1e-38 : 1);
  }
  return v;
}

// --- the two mutations, so the fixture can prove it discriminates -------------

/** `cosineNormalized` with an f32 accumulator: the naive Rust port. */
function cosineF32(a: Float32Array, b: Float32Array): number {
  let sum = 0;
  for (let i = 0; i < a.length; i++) sum = Math.fround(sum + Math.fround(a[i] * b[i]));
  return sum;
}

/** The transcription of `embedding.ts:165-173`. Disclosed: not exported, so not
 *  callable from here. Accumulates and divides in f64; the single rounding to
 *  f32 happens on store into the Float32Array. */
function normalizeLikeEmbedding(v: Float32Array): Float32Array {
  const out = new Float32Array(v);
  let sum = 0;
  for (const x of out) sum += x * x;
  const norm = Math.sqrt(sum);
  if (norm > 0) {
    for (let i = 0; i < out.length; i++) out[i] /= norm;
  }
  return out;
}

/** Normalisation with the divisor rounded to f32 first, i.e. two roundings
 *  instead of one. The other mutation this fixture must be able to fail. */
function normalizeF32Divisor(v: Float32Array): Float32Array {
  const out = new Float32Array(v);
  let sum = 0;
  for (const x of out) sum = Math.fround(sum + Math.fround(x * x));
  const norm = Math.fround(Math.sqrt(sum));
  if (norm > 0) {
    for (let i = 0; i < out.length; i++) out[i] = Math.fround(out[i] / norm);
  }
  return out;
}

// --- sections ----------------------------------------------------------------

interface CosineCase {
  label: string;
  a: string[];
  b: string[];
  expect: string;
  /** True when an f32 accumulator gives a different answer for this pair. */
  discriminates: boolean;
}

const cosineCases: CosineCase[] = [];

function addCosine(label: string, a: Float32Array, b: Float32Array): void {
  const expect = cosineNormalized(a, b);
  cosineCases.push({
    label,
    a: bitsOf(a),
    b: bitsOf(b),
    expect: f64bits(expect),
    discriminates: f64bits(cosineF32(a, b)) !== f64bits(expect),
  });
}

// Two hand-written pairs whose answers are exact, so a reviewer can check the
// fixture by eye before trusting the generated ones.
addCosine("orthogonal unit vectors", new Float32Array([1, 0]), new Float32Array([0, 1]));
addCosine("identical unit vectors", new Float32Array([1, 0]), new Float32Array([1, 0]));
addCosine("opposed unit vectors", new Float32Array([1, 0]), new Float32Array([-1, 0]));
addCosine(
  "width 5, the external-provider dimension in the TS tests",
  normalizeLikeEmbedding(new Float32Array([3, 4, 0, 0, 0])),
  normalizeLikeEmbedding(new Float32Array([0, 1, 0, 0, 0])),
);
for (const width of [384, 1536]) {
  addCosine(
    `width ${width}, random`,
    randomVector(width, 0x5eed_1234 + width),
    randomVector(width, 0xbeef_0001 + width),
  );
  addCosine(
    `width ${width}, cancelling`,
    cancellingVector(width, 0x1357_9bdf + width),
    randomVector(width, 0x2468_ace0 + width),
  );
  addCosine(
    `width ${width}, denormal lanes`,
    denormalVector(width, 0x0f0f_0f0f + width),
    randomVector(width, 0xf0f0_f0f0 + width),
  );
  addCosine(
    `width ${width}, normalised random pair`,
    normalizeLikeEmbedding(randomVector(width, 0xdead_0000 + width)),
    normalizeLikeEmbedding(randomVector(width, 0xfeed_0000 + width)),
  );
}

interface TopKCase {
  label: string;
  query: string[];
  items: { item: string; vector: string[] }[];
  topK: number;
  expectItems: string[];
  expectScores: string[];
}

const topKCases: TopKCase[] = [];

function addTopK(
  label: string,
  query: Float32Array,
  items: { item: string; vector: Float32Array }[],
  topK: number,
): void {
  const scored = topKBySimilarity(query, items, topK);
  topKCases.push({
    label,
    query: bitsOf(query),
    items: items.map((i) => ({ item: i.item, vector: bitsOf(i.vector) })),
    topK,
    expectItems: scored.map((s) => s.item),
    expectScores: scored.map((s) => f64bits(s.score)),
  });
}

addTopK(
  "ranks by descending cosine",
  new Float32Array([1, 0]),
  [
    { item: "orthogonal", vector: new Float32Array([0, 1]) },
    { item: "exact", vector: new Float32Array([1, 0]) },
    { item: "near", vector: normalizeLikeEmbedding(new Float32Array([0.9, 0.1])) },
  ],
  5,
);

// Ties keep insertion order, which is only true of a STABLE sort. This is the
// case that fails on `sort_unstable_by`.
addTopK(
  "a three-way tie keeps insertion order",
  new Float32Array([1, 0]),
  [
    { item: "first", vector: new Float32Array([1, 0]) },
    { item: "second", vector: new Float32Array([1, 0]) },
    { item: "third", vector: new Float32Array([1, 0]) },
    { item: "lower", vector: new Float32Array([0, 1]) },
  ],
  4,
);

// Ties MIXED with distinct scores, sixty of them. This case exists because two
// weaker versions of it did not work, and both looked convincing:
//
//   - A three-way tie passes on `sort_unstable_by`, because Rust's pdqsort falls
//     back to insertion sort under twenty elements and that happens to be
//     stable.
//   - A forty-way tie where EVERY score is equal also passes, because pdqsort
//     detects an all-equal partition and moves nothing.
//
// Sixty items across three score groups reorders, so this is the shape that
// actually catches an unstable sort. It was watched fail against
// `sort_unstable_by` before being kept.
addTopK(
  "sixty items over three score groups keep insertion order within each",
  new Float32Array([1, 0]),
  Array.from({ length: 60 }, (_, i) => ({
    item: `tied-${String(i).padStart(2, "0")}`,
    vector: [
      new Float32Array([1, 0]),
      normalizeLikeEmbedding(new Float32Array([0.8, 0.6])),
      new Float32Array([0, 1]),
    ][i % 3],
  })),
  60,
);

addTopK(
  "topK truncates after sorting, not before",
  new Float32Array([1, 0]),
  [
    { item: "low", vector: new Float32Array([0, 1]) },
    { item: "high", vector: new Float32Array([1, 0]) },
  ],
  1,
);

addTopK(
  "topK of zero returns nothing",
  new Float32Array([1, 0]),
  [{ item: "any", vector: new Float32Array([1, 0]) }],
  0,
);

addTopK(
  "width 384, close scores decide the order",
  randomVector(384, 0xaaaa_0001),
  [
    { item: "a", vector: randomVector(384, 0xaaaa_0002) },
    { item: "b", vector: randomVector(384, 0xaaaa_0003) },
    { item: "c", vector: cancellingVector(384, 0xaaaa_0004) },
    { item: "d", vector: denormalVector(384, 0xaaaa_0005) },
  ],
  3,
);

interface NormalizeCase {
  label: string;
  input: string[];
  expect: string[];
  discriminates: boolean;
}

const normalizeCases: NormalizeCase[] = [];

function addNormalize(label: string, input: Float32Array): void {
  const expect = normalizeLikeEmbedding(input);
  normalizeCases.push({
    label,
    input: bitsOf(input),
    expect: bitsOf(expect),
    discriminates: bitsOf(normalizeF32Divisor(input)).join() !== bitsOf(expect).join(),
  });
}

// The TS test's own case: 3-4-0-0-0 normalises to 0.6, 0.8, 0, 0, 0
// (`core/src/services/embedding.test.ts:63-70`).
addNormalize("the 3-4-0-0-0 case from embedding.test.ts", new Float32Array([3, 4, 0, 0, 0]));
addNormalize("already a unit vector", new Float32Array([1, 0, 0]));
addNormalize("a zero vector is left alone", new Float32Array([0, 0, 0]));
addNormalize("negative lanes", new Float32Array([-3, 4, -12, 0]));
addNormalize("width 384, random", randomVector(384, 0xc0ff_ee01));
addNormalize("width 384, cancelling", cancellingVector(384, 0xc0ff_ee02));
addNormalize("width 384, denormal lanes", denormalVector(384, 0xc0ff_ee03));
addNormalize("width 1536, random", randomVector(1536, 0xc0ff_ee04));

// The staleness hash. `embedSourceHash` reads the ACTIVE model identity from
// module state, which only a settings change can move, so the cases carry the
// identity explicitly and are computed with the engine's own sha256. The first
// case pins the composition itself: that what `embedSourceHash` produces for the
// default identity is `sha256(EMBED_REPO + "\n" + text)`, which is the rule the
// Rust port implements.
const defaultId = activeEmbedModelId();
if (defaultId !== EMBED_REPO) {
  throw new Error(
    `expected the default embed identity to be EMBED_REPO, got ${defaultId}. ` +
      "The staleness hash composition changed; the Rust port must change with it.",
  );
}

const hashCases = [
  { modelId: EMBED_REPO, text: "" },
  { modelId: EMBED_REPO, text: "Recipes\nPasta recipes and cooking notes." },
  { modelId: EMBED_REPO, text: memoryEmbedText("Recipes", "Pasta recipes and cooking notes.") },
  { modelId: EMBED_REPO, text: memoryEmbedText("", "") },
  { modelId: EMBED_REPO, text: memoryEmbedText("Ünïcödé Title", "Résumé of the memory.") },
  { modelId: "text-embedding-3-small", text: memoryEmbedText("Recipes", "Pasta and notes.") },
  { modelId: "text-embedding-3-large", text: memoryEmbedText("Recipes", "Pasta and notes.") },
  { modelId: "text-embedding-3-small", text: "web search\nSearch the web\ntrigger one" },
].map((c) => ({ ...c, expect: sha256HexSync(`${c.modelId}\n${c.text}`) }));

// The one case that is a true oracle for the composition rather than for sha256.
const defaultIdentityCase = {
  text: memoryEmbedText("Recipes", "Pasta recipes and cooking notes."),
  expect: embedSourceHash(memoryEmbedText("Recipes", "Pasta recipes and cooking notes.")),
};
if (defaultIdentityCase.expect !== sha256HexSync(`${EMBED_REPO}\n${defaultIdentityCase.text}`)) {
  throw new Error("embedSourceHash no longer composes as `${modelId}\\n${text}`");
}

const embedTextCases = [
  { title: "Recipes", summary: "Pasta recipes and cooking notes.", kind: "memory" },
  { title: "", summary: "", kind: "memory" },
  { title: "Multi\nline", summary: "with\nnewlines", kind: "memory" },
].map((c) => ({ ...c, expect: memoryEmbedText(c.title, c.summary) }));

// --- the discrimination check ------------------------------------------------

function assertDiscriminates(): void {
  const cosineHits = cosineCases.filter((c) => c.discriminates).length;
  const normalizeHits = normalizeCases.filter((c) => c.discriminates).length;
  // Thresholds, not "at least one": a single discriminating case could be lost
  // to an edit that looks harmless, and the whole fixture would go quiet.
  if (cosineHits < 4) {
    throw new Error(
      `only ${cosineHits} cosine cases distinguish an f32 accumulator from an f64 one. ` +
        "The fixture would pass on the broken implementation; add wider or more " +
        "cancellation-heavy pairs before writing it.",
    );
  }
  if (normalizeHits < 3) {
    throw new Error(
      `only ${normalizeHits} normalisation cases distinguish one rounding from two. ` +
        "Same problem, same fix.",
    );
  }
  console.log(
    `discrimination: ${cosineHits}/${cosineCases.length} cosine cases and ` +
      `${normalizeHits}/${normalizeCases.length} normalisation cases fail on the mutated ` +
      "implementation, so the fixture can go red.",
  );
}

assertDiscriminates();

const doc = {
  "@generated": "scripts/parity/relevance-vectors.ts. Do not edit by hand; regenerate.",
  note:
    "Floats are IEEE-754 bit patterns: f32 as 8 hex digits, f64 as 16. Compare bits, " +
    "never approximately: an approximate comparison passes on the f32-accumulating " +
    "implementation this fixture exists to catch.",
  defaultEmbedModelId: EMBED_REPO,
  cosine: cosineCases,
  topK: topKCases,
  normalize: normalizeCases,
  embedSourceHash: hashCases,
  defaultIdentityHash: defaultIdentityCase,
  memoryEmbedText: embedTextCases,
};

await Deno.mkdir(new URL(".", OUT), { recursive: true });
await Deno.writeTextFile(OUT, `${JSON.stringify(doc, null, 2)}\n`);
console.log(
  `wrote ${OUT.pathname}: ${cosineCases.length} cosine, ${topKCases.length} topK, ` +
    `${normalizeCases.length} normalize, ${hashCases.length} hash cases`,
);
