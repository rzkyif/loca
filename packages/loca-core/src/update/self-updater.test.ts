// deterministic canonicalize() and base-64 decoder.
//
// `canonicalize` is the linchpin of manifest signing: the signer canonicalizes
// `{version, binaries}` to bytes, signs, and includes the signature; the
// verifier MUST produce the same bytes for verification to pass. Any
// divergence in key ordering, escaping, or array handling would
// undetectably brick the auto-update channel for every shipped core.

import { assertEquals, assertRejects } from "@std/assert";
import { join } from "@std/path";
import { encodeBase64 } from "@std/encoding/base64";
import * as ed from "@noble/ed25519";
import type { CoreManifest } from "@loca/shared";
import {
  __resetUpdateSubscribersForTesting,
  canonicalize,
  commitRename,
  decodeBase64,
  emitUpdate,
  signedManifestPayload,
  subscribeUpdate,
  swapBinaryInPlace,
  type UpdateEvent,
  verifyCoreManifestSignature,
} from "./self-updater.ts";

Deno.test("canonicalize: object keys sorted lexicographically", () => {
  assertEquals(canonicalize({ b: 1, a: 2, c: 3 }), `{"a":2,"b":1,"c":3}`);
});

Deno.test("signedManifestPayload: covers workers + helpers, excludes signature, detects tampering", () => {
  const manifest = {
    schemaVersion: 1,
    version: "1.2.3",
    binaries: [
      {
        triple: "aarch64-apple-darwin",
        url: "https://x/c",
        sha256: "aa",
      },
    ],
    workers: [{ name: "tool-worker.ts", url: "https://x/w", sha256: "bb" }],
    helpers: [
      {
        name: "loca-core-keychain",
        triple: "aarch64-apple-darwin",
        url: "https://x/k",
        sha256: "cc",
      },
    ],
    signature: "should-not-be-signed",
  };
  const payload = signedManifestPayload(manifest);
  // Every executed field is inside the signed bytes...
  assertEquals(payload.includes("tool-worker.ts"), true);
  assertEquals(payload.includes("loca-core-keychain"), true);
  assertEquals(payload.includes("binaries"), true);
  // ...and the signature field itself is excluded.
  assertEquals(payload.includes("should-not-be-signed"), false);
  // Tampering a worker URL changes the signed bytes (so the signature fails).
  // This is the regression guard against narrowing coverage back to
  // {version, binaries}.
  const tamperedWorker = {
    ...manifest,
    workers: [{ name: "tool-worker.ts", url: "https://evil/w", sha256: "bb" }],
  };
  assertEquals(signedManifestPayload(manifest) === signedManifestPayload(tamperedWorker), false);
  // Tampering a helper hash likewise changes the signed bytes.
  const tamperedHelper = {
    ...manifest,
    helpers: [{ ...manifest.helpers[0], sha256: "ff" }],
  };
  assertEquals(signedManifestPayload(manifest) === signedManifestPayload(tamperedHelper), false);
});

// The core.json self-update signature gate governs download-and-execute of the
// Core binary + workers + helpers, so it gets the same accept/reject coverage as
// its two siblings (verifyBinariesSignature, verifyBuiltinManifestSignature). A
// throwaway keypair signs signedManifestPayload; a regression that inverted the
// check or narrowed the signed payload would fail one of these.
async function signedCoreManifest(sk: Uint8Array): Promise<CoreManifest> {
  const body: Omit<CoreManifest, "signature"> = {
    schemaVersion: 1,
    version: "9.9.9",
    binaries: [{ triple: "aarch64-apple-darwin", url: "https://x/core", sha256: "aa" }],
    workers: [{ name: "tool-worker.ts", url: "https://x/w", sha256: "bb" }],
    helpers: [
      {
        name: "loca-core-keychain",
        triple: "aarch64-apple-darwin",
        url: "https://x/k",
        sha256: "cc",
      },
    ],
  };
  const sig = await ed.signAsync(
    new TextEncoder().encode(signedManifestPayload(body as unknown as Record<string, unknown>)),
    sk,
  );
  return { ...body, signature: encodeBase64(sig) };
}

Deno.test("verifyCoreManifestSignature: a valid signature is accepted", async () => {
  const sk = ed.utils.randomSecretKey();
  const pk = await ed.getPublicKeyAsync(sk);
  const manifest = await signedCoreManifest(sk);
  assertEquals(await verifyCoreManifestSignature(manifest, pk), true);
});

Deno.test("verifyCoreManifestSignature: a tampered version is rejected", async () => {
  const sk = ed.utils.randomSecretKey();
  const pk = await ed.getPublicKeyAsync(sk);
  const manifest = await signedCoreManifest(sk);
  assertEquals(await verifyCoreManifestSignature({ ...manifest, version: "6.6.6" }, pk), false);
});

Deno.test("verifyCoreManifestSignature: a tampered (executed) worker URL is rejected", async () => {
  const sk = ed.utils.randomSecretKey();
  const pk = await ed.getPublicKeyAsync(sk);
  const manifest = await signedCoreManifest(sk);
  const tampered: CoreManifest = {
    ...manifest,
    workers: [{ ...manifest.workers[0], url: "https://evil/w" }],
  };
  assertEquals(await verifyCoreManifestSignature(tampered, pk), false);
});

Deno.test("verifyCoreManifestSignature: the wrong public key is rejected", async () => {
  const sk = ed.utils.randomSecretKey();
  const manifest = await signedCoreManifest(sk);
  const otherPk = await ed.getPublicKeyAsync(ed.utils.randomSecretKey());
  assertEquals(await verifyCoreManifestSignature(manifest, otherPk), false);
});

Deno.test("canonicalize: nested objects recursively sorted", () => {
  assertEquals(
    canonicalize({ z: { b: 1, a: 2 }, a: { y: 1, x: 2 } }),
    `{"a":{"x":2,"y":1},"z":{"a":2,"b":1}}`,
  );
});

Deno.test("canonicalize: arrays preserve order (sorting would break signature)", () => {
  assertEquals(canonicalize([3, 1, 2]), `[3,1,2]`);
});

Deno.test("canonicalize: primitives match JSON.stringify", () => {
  assertEquals(canonicalize(42), "42");
  assertEquals(canonicalize("x"), `"x"`);
  assertEquals(canonicalize(null), "null");
  assertEquals(canonicalize(true), "true");
});

Deno.test("canonicalize: strings with quotes and backslashes are escaped per JSON", () => {
  assertEquals(canonicalize(`a"b\\c`), `"a\\"b\\\\c"`);
});

Deno.test("canonicalize: realistic manifest body round-trips deterministically", () => {
  const body = {
    version: "0.2.0",
    binaries: [
      { triple: "aarch64-apple-darwin", url: "https://x/a", sha256: "00" },
      { triple: "x86_64-pc-windows-msvc", url: "https://x/w", sha256: "00" },
    ],
  };
  // Two independent invocations produce the same bytes (signing+verifying
  // both call this, so bit-for-bit equality is the contract).
  assertEquals(canonicalize(body), canonicalize(body));
  // Reordering input keys produces the same output.
  const reorder = { binaries: body.binaries, version: body.version };
  assertEquals(canonicalize(reorder), canonicalize(body));
});

Deno.test("decodeBase64: round-trips known fixture (ed25519 public key length is 32)", () => {
  // `AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=` -> 32 zero bytes.
  const out = decodeBase64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=");
  assertEquals(out.length, 32);
  for (const b of out) assertEquals(b, 0);
});

Deno.test("decodeBase64: ASCII round-trip via btoa matches Web standard", () => {
  const s = "hello";
  const b64 = btoa(s);
  const out = decodeBase64(b64);
  assertEquals(new TextDecoder().decode(out), s);
});

// --- subscribe/emit ---------------------------------------------------------

Deno.test("subscribeUpdate: subscriber receives emitted staged event", () => {
  __resetUpdateSubscribersForTesting();
  const received: UpdateEvent[] = [];
  const unsubscribe = subscribeUpdate((e) => received.push(e));
  try {
    emitUpdate({ kind: "staged", version: "1.2.3" });
    assertEquals(received, [{ kind: "staged", version: "1.2.3" }]);
  } finally {
    unsubscribe();
  }
});

Deno.test("subscribeUpdate: multiple subscribers each receive", () => {
  __resetUpdateSubscribersForTesting();
  const a: UpdateEvent[] = [];
  const b: UpdateEvent[] = [];
  const unsubA = subscribeUpdate((e) => a.push(e));
  const unsubB = subscribeUpdate((e) => b.push(e));
  try {
    emitUpdate({
      kind: "error",
      code: "update_failed",
      message: "boom",
    });
    assertEquals(a.length, 1);
    assertEquals(b.length, 1);
    assertEquals(a[0], b[0]);
  } finally {
    unsubA();
    unsubB();
  }
});

Deno.test("subscribeUpdate: unsubscribe stops delivery", () => {
  __resetUpdateSubscribersForTesting();
  const received: UpdateEvent[] = [];
  const unsubscribe = subscribeUpdate((e) => received.push(e));
  emitUpdate({ kind: "staged", version: "1" });
  unsubscribe();
  emitUpdate({ kind: "staged", version: "2" });
  assertEquals(received, [{ kind: "staged", version: "1" }]);
});

Deno.test("subscribeUpdate: a throwing subscriber does not block others", () => {
  __resetUpdateSubscribersForTesting();
  const received: UpdateEvent[] = [];
  const unsubA = subscribeUpdate(() => {
    throw new Error("subscriber threw");
  });
  const unsubB = subscribeUpdate((e) => received.push(e));
  try {
    emitUpdate({ kind: "staged", version: "ok" });
    assertEquals(received, [{ kind: "staged", version: "ok" }]);
  } finally {
    unsubA();
    unsubB();
  }
});

Deno.test("__resetUpdateSubscribersForTesting: clears the subscriber set", () => {
  __resetUpdateSubscribersForTesting();
  const received: UpdateEvent[] = [];
  subscribeUpdate((e) => received.push(e));
  __resetUpdateSubscribersForTesting();
  emitUpdate({ kind: "staged", version: "1" });
  assertEquals(received, []);
});

// --- in-place binary swap ---------------------------------------------------
//
// The swap runs while core is still executing `current`, so it must always leave
// a runnable binary at `current` and preserve the previous one at `<current>.old`
// (the anchor boot-time rollback in rollback.ts restores). `onWindows` is passed
// explicitly so both branches run on any host.

async function tmpBinDir(): Promise<string> {
  return await Deno.makeTempDir({ prefix: "loca-swap-test-" });
}

Deno.test("swapBinaryInPlace (unix): installs staged and anchors the old binary", async () => {
  const dir = await tmpBinDir();
  try {
    const current = join(dir, "loca-core");
    const staged = join(dir, "staging", "loca-core-2");
    await Deno.mkdir(join(dir, "staging"));
    await Deno.writeTextFile(current, "v1");
    await Deno.writeTextFile(staged, "v2");

    await swapBinaryInPlace(staged, current, false);

    assertEquals(await Deno.readTextFile(current), "v2");
    // The previous binary survives as the rollback anchor.
    assertEquals(await Deno.readTextFile(`${current}.old`), "v1");
    // The staged copy was consumed by the rename.
    assertEquals(await pathGone(staged), true);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("swapBinaryInPlace (unix): a stale anchor from a prior update is replaced", async () => {
  const dir = await tmpBinDir();
  try {
    const current = join(dir, "loca-core");
    const staged = join(dir, "staged");
    await Deno.writeTextFile(current, "v2");
    await Deno.writeTextFile(staged, "v3");
    await Deno.writeTextFile(`${current}.old`, "v1-stale");

    await swapBinaryInPlace(staged, current, false);

    assertEquals(await Deno.readTextFile(current), "v3");
    assertEquals(await Deno.readTextFile(`${current}.old`), "v2");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("swapBinaryInPlace (windows): renames the running exe aside, then installs", async () => {
  const dir = await tmpBinDir();
  try {
    const current = join(dir, "loca-core.exe");
    const staged = join(dir, "loca-core-2.exe");
    await Deno.writeTextFile(current, "v1");
    await Deno.writeTextFile(staged, "v2");

    await swapBinaryInPlace(staged, current, true);

    assertEquals(await Deno.readTextFile(current), "v2");
    assertEquals(await Deno.readTextFile(`${current}.old`), "v1");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("swapBinaryInPlace (windows): a failed install rename restores the running exe", async () => {
  const dir = await tmpBinDir();
  try {
    const current = join(dir, "loca-core.exe");
    const missingStaged = join(dir, "does-not-exist.exe");
    await Deno.writeTextFile(current, "v1");

    // rename(staged -> current) fails because staged is absent; the swap must
    // put the running binary back so the supervisor still finds one.
    await assertRejects(() => swapBinaryInPlace(missingStaged, current, true));

    assertEquals(await Deno.readTextFile(current), "v1");
    assertEquals(await pathGone(`${current}.old`), true);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("commitRename: replaces the target with the staged artifact", async () => {
  const dir = await tmpBinDir();
  try {
    const to = join(dir, "loca-core-keychain");
    const from = join(dir, "loca-core-keychain.new");
    await Deno.writeTextFile(to, "old");
    await Deno.writeTextFile(from, "new");

    await commitRename(from, to, false);

    assertEquals(await Deno.readTextFile(to), "new");
    assertEquals(await pathGone(from), true);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("commitRename (windows): a locked target is moved aside, not overwritten", async () => {
  const dir = await tmpBinDir();
  try {
    // Simulate a target the direct rename can't replace by making it a
    // non-empty directory: rename(file -> dir) fails, so the move-aside path
    // (rename target -> target.old, then staged -> target) runs.
    const to = join(dir, "ptyhost.exe");
    await Deno.mkdir(to);
    await Deno.writeTextFile(join(to, "keep"), "busy");
    const from = join(dir, "ptyhost.new.exe");
    await Deno.writeTextFile(from, "new");

    await commitRename(from, to, true);

    assertEquals(await Deno.readTextFile(to), "new");
    // The previous target survives aside so a running helper keeps its image.
    assertEquals(await Deno.readTextFile(join(`${to}.old`, "keep")), "busy");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

async function pathGone(p: string): Promise<boolean> {
  try {
    await Deno.stat(p);
    return false;
  } catch {
    return true;
  }
}
