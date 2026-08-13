// extension installer: local download path (no network, no npm tarball fetch).
// Drives the path from source dir -> registry rows via the real `startDownload`
// (a no-dep extension finishes at 'installed'; a deps-bearing one stops at
// 'downloaded'). The npm tarball + `deno install` paths are covered by
// integration in real installs; isolating them would require a fixture server.

import { assertEquals, assertExists, assertRejects, assertThrows } from "@std/assert";
import { join } from "@std/path";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { paths } from "../paths.ts";
import { encodeBase64 } from "@std/encoding/base64";
import { encodeHex } from "@std/encoding/hex";
import {
  extractableEntryType,
  flattenNpmName,
  flattenPermissions,
  type InstallEventSink,
  isWithin,
  startDownload,
  verifyTarball,
} from "./installer.ts";
import { AppError } from "@loca/core-engine";
import { extensionsRegistry } from "./registry.ts";

// --- Pure helper tests ------------------------------------------------------

Deno.test("flattenNpmName: scope rewrite", () => {
  assertEquals(flattenNpmName("plain"), "plain");
  assertEquals(flattenNpmName("@loca/builtin"), "@loca__builtin");
});

Deno.test("flattenPermissions: walks every documented kind", () => {
  const out = flattenPermissions({
    net: [{ host: "api.example", reason: "fetch", ports: [443] }],
    read: [{ path: "$home/x", reason: "config" }],
    write: [{ path: "$downloads", reason: "save" }],
    run: [{ binary: "git", reason: "vcs" }],
    env: [{ key: "OPENAI_KEY", reason: "auth" }],
    ffi: [{ reason: "native" }],
    sys: [{ flag: "hostname", reason: "ident" }],
  });
  const kinds = out.map((p) => p.kind).sort();
  assertEquals(kinds, ["env", "ffi", "net", "read", "run", "sys", "write"]);
});

Deno.test("flattenPermissions: returns empty array when permissions object is absent", () => {
  assertEquals(flattenPermissions(undefined), []);
});

Deno.test("isWithin: accepts in-tree paths, rejects zip-slip traversal", () => {
  const root = "/tmp/extension";
  // In-tree entries are allowed.
  assertEquals(isWithin(root, join(root, "loca.json")), true);
  assertEquals(isWithin(root, join(root, "src/index.ts")), true);
  assertEquals(isWithin(root, root), true);
  // A `package/`-stripped traversal entry must be rejected.
  assertEquals(isWithin(root, join(root, "../../../bin/loca-core")), false);
  assertEquals(isWithin(root, join(root, "../sibling")), false);
  // A sibling dir that merely shares a prefix is NOT inside root.
  assertEquals(isWithin(root, "/tmp/extension-evil/x"), false);
  // Absolute escape outside root.
  assertEquals(isWithin(root, "/etc/passwd"), false);
});

Deno.test("extractableEntryType: only regular files + dirs are extractable", () => {
  // Regular-file typeflags.
  assertEquals(extractableEntryType("0"), "file");
  assertEquals(extractableEntryType("\0"), "file");
  assertEquals(extractableEntryType(""), "file");
  // Directory.
  assertEquals(extractableEntryType("5"), "dir");
  // Symlink / hardlink / char-device / fifo must be refused (sandbox escape).
  assertEquals(extractableEntryType("2"), null); // symlink
  assertEquals(extractableEntryType("1"), null); // hardlink
  assertEquals(extractableEntryType("3"), null); // char device
  assertEquals(extractableEntryType("6"), null); // fifo
});

Deno.test("verifyTarball: accepts a matching sha512 SRI integrity", async () => {
  const bytes = new TextEncoder().encode("tarball-contents");
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-512", bytes.buffer as ArrayBuffer));
  const integrity = `sha512-${encodeBase64(digest)}`;
  // Resolves (no throw) on a correct digest.
  await verifyTarball(bytes, "https://reg/x.tgz", { integrity });
});

Deno.test("verifyTarball: rejects a mismatched sha512 integrity", async () => {
  const bytes = new TextEncoder().encode("tarball-contents");
  const wrong = `sha512-${encodeBase64(new Uint8Array(64))}`; // all-zero digest
  await assertRejects(
    () => verifyTarball(bytes, "https://reg/x.tgz", { integrity: wrong }),
    AppError,
    "sha512",
  );
});

Deno.test("verifyTarball: falls back to the legacy sha1 shasum", async () => {
  const bytes = new TextEncoder().encode("tarball-contents");
  const sha1 = encodeHex(
    new Uint8Array(await crypto.subtle.digest("SHA-1", bytes.buffer as ArrayBuffer)),
  );
  await verifyTarball(bytes, "https://reg/x.tgz", { shasum: sha1 }); // matches -> ok
  await assertRejects(
    () => verifyTarball(bytes, "https://reg/x.tgz", { shasum: "0".repeat(40) }),
    AppError,
    "sha1",
  );
});

Deno.test("verifyTarball: accepts a matching sha256 hex (built-in extension path)", async () => {
  const bytes = new TextEncoder().encode("tarball-contents");
  const sha256 = encodeHex(
    new Uint8Array(await crypto.subtle.digest("SHA-256", bytes.buffer as ArrayBuffer)),
  );
  await verifyTarball(bytes, "https://cdn/x.tgz", { sha256 }); // matches -> ok
  await assertRejects(
    () => verifyTarball(bytes, "https://cdn/x.tgz", { sha256: "0".repeat(64) }),
    AppError,
    "sha256",
  );
});

Deno.test("verifyTarball: no integrity metadata fails closed (throws)", async () => {
  const bytes = new TextEncoder().encode("tarball-contents");
  // A supply-chain integrity check must not silently downgrade to "unverified"
  // when the (untrusted) registry metadata omits both integrity and shasum.
  await assertRejects(
    () => verifyTarball(bytes, "https://reg/x.tgz", undefined),
    AppError,
    "unverified",
  );
  await assertRejects(() => verifyTarball(bytes, "https://reg/x.tgz", {}), AppError, "unverified");
});

Deno.test("startDownload(local): rejects a path-traversal slug before doing any work", () => {
  // The slug becomes a path segment under the extensions dir; a `..`/separator
  // slug must be refused so it can't escape (e.g. overwrite a core binary).
  for (const slug of ["../../bin/loca-core", "..", "a/b", "with space", "x".repeat(65)]) {
    assertThrows(
      () => startDownload({ source: "local", path: "/tmp/whatever", slug }),
      AppError,
      "invalid extension slug",
    );
  }
});

// --- Local-install integration ----------------------------------------------

const MIN_MANIFEST = JSON.stringify({
  $schema: "https://get.loca.rzkyif.com/schemas/loca-v1.json",
  name: "mini",
  displayName: "mini",
  description: "tiny test extension",
  tools: [
    {
      name: "noop",
      description: "does nothing",
      function: "noop",
      parameters: { type: "object", properties: {} },
    },
  ],
});

async function awaitDone(
  sink: InstallEventSink & { result: Promise<{ ok: boolean; code: number }> },
): Promise<{ ok: boolean; code: number }> {
  return await sink.result;
}

function makeRecordingSink() {
  let done!: (r: { ok: boolean; code: number }) => void;
  const result = new Promise<{ ok: boolean; code: number }>((r) => {
    done = r;
  });
  const logs: Array<{ stream: string; line: string }> = [];
  const sink: InstallEventSink & {
    result: Promise<{ ok: boolean; code: number }>;
    logs: typeof logs;
  } = {
    log(_jobId, _id, stream, line) {
      logs.push({ stream, line });
    },
    done(_jobId, _id, ok, code) {
      done({ ok, code });
    },
    result,
    logs,
  };
  return sink;
}

Deno.test("startDownload(local, no deps): finishes the lifecycle at 'installed', hash pinned", async () => {
  const env = await setupTestEnv();
  try {
    // Synthesize a "local" extension on disk with loca.json + index.ts (no deps).
    const srcDir = await Deno.makeTempDir({ prefix: "loca-tk-src-" });
    await Deno.writeTextFile(join(srcDir, "loca.json"), MIN_MANIFEST);
    await Deno.writeTextFile(
      join(srcDir, "index.ts"),
      `export async function noop() { return null; }\n`,
    );

    const sink = makeRecordingSink();
    const { extensionId } = startDownload(
      {
        source: "local",
        path: srcDir,
        slug: "mini-test",
      },
      sink,
    );
    assertEquals(extensionId, "mini-test");
    assertEquals((await awaitDone(sink)).ok, true);

    // No declared deps -> the download itself pins the hash and flips the row to
    // 'installed' (no separate Install step needed).
    const tk = extensionsRegistry().get("mini-test");
    assertExists(tk);
    assertEquals(tk?.displayName, "mini");
    assertEquals(tk?.version, "local");
    assertEquals(tk?.status, "installed");
    assertEquals(tk?.hasDeps, false);
    assertEquals((tk?.contentHash ?? "").length > 0, true);
    const tools = extensionsRegistry().listTools("mini-test");
    assertEquals(tools.length, 1);
    assertEquals(tools[0].name, "noop");

    // Files landed at the install path, byte-identical (no deno.json injected).
    const installed = join(paths().extensionsDir, "mini-test");
    assertEquals(await Deno.readTextFile(join(installed, "loca.json")), MIN_MANIFEST);
    await assertRejects(() => Deno.stat(join(installed, "deno.json")));
  } finally {
    await env.teardown();
  }
});

Deno.test("startDownload(local, declared deps): stays 'downloaded' until install", async () => {
  const env = await setupTestEnv();
  try {
    const srcDir = await Deno.makeTempDir({ prefix: "loca-tk-deps-" });
    await Deno.writeTextFile(join(srcDir, "loca.json"), MIN_MANIFEST);
    await Deno.writeTextFile(
      join(srcDir, "index.ts"),
      `export async function noop() { return null; }\n`,
    );
    // A deno.json with an npm: import means deps must be installed first, so the
    // download stops at 'downloaded' (the explicit Install step runs deno install).
    await Deno.writeTextFile(
      join(srcDir, "deno.json"),
      JSON.stringify({ imports: { "mime-types": "npm:mime-types@^3.0.0" } }),
    );

    const sink = makeRecordingSink();
    startDownload({ source: "local", path: srcDir, slug: "deps-test" }, sink);
    assertEquals((await awaitDone(sink)).ok, true);

    const tk = extensionsRegistry().get("deps-test");
    assertExists(tk);
    assertEquals(tk?.status, "downloaded");
    assertEquals(tk?.hasDeps, true);
    assertEquals(tk?.contentHash, "");
  } finally {
    await env.teardown();
  }
});

Deno.test("startDownload(local): missing loca.json fails, writes no registry row", async () => {
  const env = await setupTestEnv();
  try {
    const srcDir = await Deno.makeTempDir({ prefix: "loca-tk-bad-" });
    // No loca.json at all.
    await Deno.writeTextFile(join(srcDir, "index.ts"), `export const x = 1;`);

    const sink = makeRecordingSink();
    startDownload({ source: "local", path: srcDir, slug: "bad" }, sink);
    assertEquals((await awaitDone(sink)).ok, false);

    // No registry row written.
    assertEquals(extensionsRegistry().get("bad"), undefined);
  } finally {
    await env.teardown();
  }
});

Deno.test("startDownload(local): malformed loca.json fails", async () => {
  const env = await setupTestEnv();
  try {
    const srcDir = await Deno.makeTempDir({ prefix: "loca-tk-malformed-" });
    await Deno.writeTextFile(join(srcDir, "loca.json"), `{ not valid json`);

    const sink = makeRecordingSink();
    startDownload({ source: "local", path: srcDir, slug: "malformed" }, sink);
    assertEquals((await awaitDone(sink)).ok, false);
  } finally {
    await env.teardown();
  }
});
