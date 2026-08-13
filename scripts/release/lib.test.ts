// Tests for mergeVersionHistory, the pure retention logic that decides which
// versioned R2 artifacts to keep and which to evict on each release, plus the
// version-stripping normalizers that keep a lone version bump from re-triggering
// a release (source hashes are version-blind).

import { assertEquals, assertNotEquals } from "@std/assert";
import { join } from "@std/path";

import { hashPaths, mergeVersionHistory, stripCoreVersion, stripJsonVersion } from "./lib.ts";

Deno.test("first release: one entry, nothing evicted", () => {
  const r = mergeVersionHistory(undefined, "1.0.0", ["1.0.0/a", "1.0.0/b"], 2);
  assertEquals(r.history, [{ version: "1.0.0", keys: ["1.0.0/a", "1.0.0/b"] }]);
  assertEquals(r.evictedKeys, []);
});

Deno.test("new version goes to the front; within cap nothing evicts", () => {
  const prior = [{ version: "1.0.0", keys: ["1.0.0/a"] }];
  const r = mergeVersionHistory(prior, "1.0.1", ["1.0.1/a"], 2);
  assertEquals(r.history, [
    { version: "1.0.1", keys: ["1.0.1/a"] },
    { version: "1.0.0", keys: ["1.0.0/a"] },
  ]);
  assertEquals(r.evictedKeys, []);
});

Deno.test("exceeding cap evicts the oldest version's keys", () => {
  const prior = [
    { version: "1.0.1", keys: ["1.0.1/a"] },
    { version: "1.0.0", keys: ["1.0.0/a", "1.0.0/b"] },
  ];
  const r = mergeVersionHistory(prior, "1.0.2", ["1.0.2/a"], 2);
  assertEquals(r.history, [
    { version: "1.0.2", keys: ["1.0.2/a"] },
    { version: "1.0.1", keys: ["1.0.1/a"] },
  ]);
  assertEquals(r.evictedKeys, ["1.0.0/a", "1.0.0/b"]);
});

Deno.test("same version republished: keys union in place, no eviction", () => {
  const prior = [
    { version: "1.0.1", keys: ["1.0.1/mac"] },
    { version: "1.0.0", keys: ["1.0.0/a"] },
  ];
  // Platform-fill: a new windows bundle plus a re-uploaded mac bundle.
  const r = mergeVersionHistory(prior, "1.0.1", ["1.0.1/mac", "1.0.1/win"], 2);
  assertEquals(r.history, [
    { version: "1.0.1", keys: ["1.0.1/mac", "1.0.1/win"] },
    { version: "1.0.0", keys: ["1.0.0/a"] },
  ]);
  assertEquals(r.evictedKeys, []);
});

Deno.test("duplicate keys within a single record are deduped", () => {
  const r = mergeVersionHistory(undefined, "1.0.0", ["1.0.0/a", "1.0.0/a"], 2);
  assertEquals(r.history, [{ version: "1.0.0", keys: ["1.0.0/a"] }]);
  assertEquals(r.evictedKeys, []);
});

Deno.test("stripJsonVersion: differing versions normalize to the same text", () => {
  const a = `{\n  "name": "x",\n  "version": "0.1.2",\n  "exports": "./mod.ts"\n}\n`;
  const b = `{\n  "name": "x",\n  "version": "9.9.9",\n  "exports": "./mod.ts"\n}\n`;
  assertEquals(stripJsonVersion(a), stripJsonVersion(b));
  // ...but a real content change survives the strip.
  const c = a.replace(`"exports": "./mod.ts"`, `"exports": "./other.ts"`);
  assertNotEquals(stripJsonVersion(a), stripJsonVersion(c));
});

Deno.test("stripJsonVersion: only the top-level version is blanked", () => {
  const text = `{\n  "version": "1.2.3",\n  "dep": { "version": "1.2.3" }\n}`;
  const out = stripJsonVersion(text);
  // The top-level value is replaced; the same-valued nested one is untouched.
  assertEquals(out, `{\n  "version": "0.0.0",\n  "dep": { "version": "1.2.3" }\n}`);
});

Deno.test("stripCoreVersion: differing CORE_VERSION normalize the same", () => {
  const a = `export const CORE_VERSION = "0.1.3";\nexport const X = "keep";\n`;
  const b = `export const CORE_VERSION = "2.0.0";\nexport const X = "keep";\n`;
  assertEquals(stripCoreVersion(a), stripCoreVersion(b));
  const c = a.replace(`"keep"`, `"changed"`);
  assertNotEquals(stripCoreVersion(a), stripCoreVersion(c));
});

Deno.test("hashPaths transform: a version-only bump does not change the hash", async () => {
  const dir = await Deno.makeTempDir({ prefix: "loca-hashpaths-" });
  try {
    const file = join(dir, "deno.json");
    await Deno.writeTextFile(file, `{\n  "version": "0.1.0",\n  "x": 1\n}\n`);
    const h1 = await hashPaths([{ path: file, transform: stripJsonVersion }]);

    // Version-only bump: same hash.
    await Deno.writeTextFile(file, `{\n  "version": "0.1.1",\n  "x": 1\n}\n`);
    const h2 = await hashPaths([{ path: file, transform: stripJsonVersion }]);
    assertEquals(h1, h2);

    // Real content change: different hash.
    await Deno.writeTextFile(file, `{\n  "version": "0.1.1",\n  "x": 2\n}\n`);
    const h3 = await hashPaths([{ path: file, transform: stripJsonVersion }]);
    assertNotEquals(h2, h3);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});
