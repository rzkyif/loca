// source-string parser for the HF-spec format `@user/repo/branch/file`.
//
// `parseSource` is the single seam every other download helper trusts to
// translate user-supplied source strings into (relPath, url, filename), so
// the edge cases are worth pinning down.

import { assert, assertEquals, assertThrows } from "@std/assert";
import { parseSource, probeSource } from "./sources.ts";

Deno.test("parseSource: HF spec -> resolve URL + relPath stripped of branch", () => {
  const out = parseSource("@user/repo/main/file.gguf");
  assertEquals(out.relPath, "user/repo/file.gguf");
  assertEquals(out.filename, "file.gguf");
  assertEquals(out.url, "https://huggingface.co/user/repo/resolve/main/file.gguf?download=true");
});

Deno.test("parseSource: nested filenames keep their full path under repo", () => {
  const out = parseSource("@user/repo/main/dir/sub/file.bin");
  assertEquals(out.relPath, "user/repo/dir/sub/file.bin");
  // Filename is the basename only. The path lives in relPath.
  assertEquals(out.filename, "file.bin");
});

Deno.test("parseSource: filename containing slashes resolves correctly", () => {
  const out = parseSource("@u/r/b/x.json");
  assertEquals(out.filename, "x.json");
});

Deno.test("parseSource: non-`@` source treated as absolute path, url is empty", () => {
  const out = parseSource("/abs/path/to/file.gguf");
  assertEquals(out.relPath, "/abs/path/to/file.gguf");
  assertEquals(out.url, "");
  assertEquals(out.filename, "file.gguf");
});

Deno.test("parseSource: rejects HF specs missing branch or file", () => {
  assertThrows(() => parseSource("@u/r"), Error, "invalid source format");
  assertThrows(() => parseSource("@u/r/b"), Error, "invalid source format");
});

Deno.test("parseSource: rejects path-traversal components", () => {
  // A `..` in any component would let the joined relPath escape the
  // destination root.
  assertThrows(() => parseSource("@../r/b/file"), Error, "unsafe source component");
  assertThrows(() => parseSource("@u/../b/file"), Error, "unsafe source component");
  assertThrows(() => parseSource("@u/r/b/../escape"), Error, "unsafe source component");
  assertThrows(() => parseSource("@u/r/b/../../etc/passwd"), Error, "unsafe source component");
});

// Guards the no-non-consented-network invariant at its source: the always-on
// requirements snapshot probes with network:false (client connect + settings
// broadcast must not touch the network, per the AGENTS.md/SECURITY.md rule). If a
// regression re-enabled the boot-path HEAD, `fetch` would be called and this
// fails. The network:true case proves the flag actually gates (not a no-op).
Deno.test("probeSource: network:false does NOT touch the network for a missing @-source", async () => {
  const dir = await Deno.makeTempDir({ prefix: "loca-probe-test-" });
  const origFetch = globalThis.fetch;
  let fetchCalls = 0;
  globalThis.fetch = ((..._args: unknown[]) => {
    fetchCalls++;
    throw new Error("no-non-consented-network: probeSource must not fetch on the boot path");
  }) as typeof fetch;
  try {
    const r = await probeSource("@user/repo/main/missing.gguf", dir, { network: false });
    assertEquals(fetchCalls, 0, "probeSource(network:false) must make no outbound request");
    assertEquals(r.alreadyHave, false); // presence is a local check, still accurate
    assertEquals(r.sizeBytes, undefined); // no size hint without a probe
  } finally {
    globalThis.fetch = origFetch;
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("probeSource: network:true DOES attempt the size HEAD (the flag really gates)", async () => {
  const dir = await Deno.makeTempDir({ prefix: "loca-probe-test-" });
  const origFetch = globalThis.fetch;
  let fetchCalls = 0;
  globalThis.fetch = ((..._args: unknown[]) => {
    fetchCalls++;
    // Probe failures are non-fatal; probeSource swallows them and returns no size.
    return Promise.reject(new Error("stubbed network"));
  }) as typeof fetch;
  try {
    const r = await probeSource("@user/repo/main/missing.gguf", dir, { network: true });
    assert(fetchCalls > 0, "probeSource(network:true) must attempt the size HEAD");
    assertEquals(r.alreadyHave, false);
  } finally {
    globalThis.fetch = origFetch;
    await Deno.remove(dir, { recursive: true });
  }
});
