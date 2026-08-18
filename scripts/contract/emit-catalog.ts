// Emits the two data files D6 needs from `@loca/model-catalog` and
// `@loca/shared`, for `packages/loca-core-rs/src/models/`.
//
// Run via `deno task contract`, not directly.
//
//   catalog.dev.json            the compiled dev-channel catalog payload
//   testdata/canonical-vectors.json  the canonicalisation and signature fixture
//
// Why the first one exists: `loadModelCatalog` short-circuits on the dev
// channel and compiles the in-repo families directly, because there is no
// published `manifests/dev/catalog.json` and the from-source build is the trust
// anchor. Rust cannot import a TypeScript module, so the payload has to reach
// the core as data. It has the exact hazard the contract header names: the Rust
// compiles fine against last week's catalog, so a model added to the authoring
// package would be silently absent from every dev core and from every test
// built on one, including the fit suite whose assertions name concrete models.
//
// Why the second one exists: the signed catalog's signature is Ed25519 over
// `canonicalize(payload)`, and `canonicalize` renders every scalar with
// `JSON.stringify`. `serde_json` renders the f64 4.0 as `4.0` where JavaScript
// renders it as `4`, so a naive Rust re-serialisation produces a different
// message and fails every signature. That path runs only on the stable and
// latest channels, so neither the dev channel nor the conformance suite would
// ever reach it. This fixture is what makes the two implementations comparable:
// the canonical form is pinned by digest, and a real signature over it is
// verified from Rust.

import * as ed from "@noble/ed25519";

import { buildCatalogPayload } from "../../packages/loca-model-catalog/src/index.ts";
import { canonicalize } from "../../packages/loca-shared/src/crypto/canonical.ts";

const [catalogOut, vectorsOut] = Deno.args;
if (!catalogOut || !vectorsOut) {
  throw new Error("usage: emit-catalog.ts <catalog.dev.json> <canonical-vectors.json>");
}

const GENERATED_BY = "deno task contract, via scripts/contract/emit-catalog.ts. Do not edit.";

// `generatedAt` is stamped by the core at load time, the same way the
// TypeScript stamps `new Date().toISOString()` on every dev-channel load, so
// the committed payload carries an empty string rather than a build date that
// would churn this file on every run.
const payload = buildCatalogPayload("");

async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function base64(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes));
}

// A fixed seed, so the signature is stable across runs and this file does not
// churn. Ed25519 signing is deterministic given the key and the message, so the
// only source of drift left is a real change to the catalog or to
// `canonicalize`, which is exactly what should show up in a diff.
const seed = new Uint8Array(
  await crypto.subtle.digest("SHA-256", new TextEncoder().encode("loca d6 canonical vectors")),
);

// Small shapes whose canonical form is readable, so a failure names the pass
// that broke rather than diffing 37 KB. Each one covers a rule `canonicalize`
// applies: key sorting is recursive, arrays keep their order, nested objects
// are sorted at every level, and strings go through `JSON.stringify` escaping.
const samples: { label: string; value: unknown }[] = [
  { label: "keys sort, recursively", value: { b: 1, a: { d: 2, c: 3 } } },
  { label: "arrays keep their order", value: { z: [3, 1, 2], a: [{ b: 1, a: 2 }] } },
  { label: "empty containers", value: { a: {}, b: [] } },
  { label: "null and booleans", value: { a: null, b: true, c: false } },
  {
    label: "string escaping",
    value: { s: 'quote " backslash \\ newline \n tab \t control  unicode é emoji \u{1f600}' },
  },
  { label: "a bare scalar is not an object", value: 42 },
  { label: "a bare string", value: "hello" },
  { label: "keys are sorted by code unit, not locale", value: { Z: 1, a: 2, B: 3, "1": 4 } },
  // `Object.keys().sort()` orders by UTF-16 CODE UNIT, and a supplementary
  // character is a surrogate pair whose lead unit (0xD83D here) sorts BELOW
  // 0xFFFD. Rust's `BTreeMap<String, _>` orders by UTF-8 bytes, where U+FFFD
  // (ef bf bd) sorts below U+1F600 (f0 9f 98 80): the opposite answer. No
  // catalog key is non-ASCII, so this case exists to keep the comparator honest
  // rather than because the catalog needs it.
  { label: "keys sort by UTF-16 code unit, not by UTF-8 byte", value: { "�": 1, "\u{1f600}": 2 } },
];

// The rendering table. `input` is the literal a JSON parser sees, so a value
// JSON cannot round-trip (negative zero) still has a case; `output` is what
// `JSON.stringify` produces for it, which is what the canonical form must
// carry. The exponent thresholds are the only places JavaScript and Rust's
// `Display` for f64 disagree, and they are the reason this table is not just
// the values the catalog happens to use.
const NUMBER_INPUTS = [
  "0",
  "-0",
  "1",
  "4",
  "-17",
  "0.1",
  "0.5",
  "16.3",
  "1.05",
  "30.5",
  "0.3333333333333333",
  "1500000000",
  "9007199254740991",
  "1e20",
  "1e21",
  "1e-6",
  "1e-7",
  "1.7976931348623157e308",
  "5e-324",
  "123456789012345680000",
  "-1.5e-9",
];

const numbers = NUMBER_INPUTS.map((input) => ({
  input,
  output: JSON.stringify(JSON.parse(input) as number),
}));

// Every number literal the real catalog contains, so the table above is not the
// only evidence: if a catalog value ever lands outside the range the rendering
// rules cover, this catches it in the digest rather than in a failed signature
// on a user's machine.
const canonical = canonicalize(payload);

const publicKey = await ed.getPublicKeyAsync(seed);
const signature = await ed.signAsync(new TextEncoder().encode(canonical), seed);

await Deno.writeTextFile(
  catalogOut,
  JSON.stringify({ generatedBy: GENERATED_BY, payload }, null, 2) + "\n",
);

await Deno.writeTextFile(
  vectorsOut,
  JSON.stringify(
    {
      generatedBy: GENERATED_BY,
      // The dev payload's canonical form, pinned by digest rather than in full:
      // 37 KB of exact text in a fixture is unreadable, and the samples below
      // are what make a failure diagnosable.
      catalogCanonicalSha256: await sha256Hex(canonical),
      // UTF-8 BYTES, via TextEncoder. `String.length` counts UTF-16 code
      // units, which differs from what Rust's `str::len` reports the moment the
      // catalog holds a non-ASCII character (a voice label does), and a length
      // check that disagrees for a reason unrelated to the canonical form is
      // worse than no length check.
      catalogCanonicalBytes: new TextEncoder().encode(canonical).length,
      publicKey: base64(publicKey),
      signature: base64(signature),
      samples: samples.map((s) => ({
        label: s.label,
        value: s.value,
        canonical: canonicalize(s.value),
      })),
      numbers,
    },
    null,
    2,
  ) + "\n",
);

const models = payload.families.reduce((n, f) => n + f.models.length, 0);
console.log(
  `catalog: ${models} llm / ${payload.stt.models.length} stt / ${payload.tts.models.length} tts ` +
    `models, ${canonical.length} canonical bytes, ${numbers.length} number vectors`,
);
