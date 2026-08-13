// Pure sign/verify + shape-assertion tests for a seeded extension manifest.
// The signature is Ed25519 over canonicalize(manifest without `signature`),
// matching the release signer (scripts/release/extension.ts).

import { assertEquals, assertThrows } from "@std/assert";
import { join } from "@std/path";
import * as ed from "@noble/ed25519";
import { encodeBase64 } from "@std/encoding/base64";
import { BUILTIN_EXTENSION_ID, canonicalize } from "@loca/shared";
import type { BuiltinExtensionManifest } from "@loca/shared";
import {
  assertManifestShape,
  readPlantedManifest,
  verifyBuiltinManifestSignature,
} from "./seeded-manifest.ts";
import { AppError } from "@loca/core-engine";

async function sign(version: string, sk: Uint8Array): Promise<BuiltinExtensionManifest> {
  const unsigned = {
    schemaVersion: 1 as const,
    version,
    id: BUILTIN_EXTENSION_ID,
    tarballUrl: "https://cdn/x.tgz",
    sha256: "a".repeat(64),
  };
  const sig = await ed.signAsync(new TextEncoder().encode(canonicalize(unsigned)), sk);
  return { ...unsigned, signature: encodeBase64(sig) };
}

Deno.test("verifyBuiltinManifestSignature: round-trips a valid signature", async () => {
  const sk = ed.utils.randomSecretKey();
  const pk = await ed.getPublicKeyAsync(sk);
  assertEquals(await verifyBuiltinManifestSignature(await sign("1.2.3", sk), pk), true);
});

Deno.test("verifyBuiltinManifestSignature: rejects a tampered manifest", async () => {
  const sk = ed.utils.randomSecretKey();
  const pk = await ed.getPublicKeyAsync(sk);
  const m = await sign("1.2.3", sk);
  m.version = "9.9.9"; // tamper after signing
  assertEquals(await verifyBuiltinManifestSignature(m, pk), false);
});

Deno.test("verifyBuiltinManifestSignature: rejects a different signer's key", async () => {
  const sk = ed.utils.randomSecretKey();
  const otherPk = await ed.getPublicKeyAsync(ed.utils.randomSecretKey());
  assertEquals(await verifyBuiltinManifestSignature(await sign("1.0.0", sk), otherPk), false);
});

Deno.test("assertManifestShape: accepts a well-formed manifest", () => {
  assertManifestShape({
    schemaVersion: 1,
    version: "1.0.0",
    id: BUILTIN_EXTENSION_ID,
    tarballUrl: "https://cdn/x.tgz",
    sha256: "a".repeat(64),
    signature: "sig",
  });
});

Deno.test("readPlantedManifest: fails closed (null) for missing, malformed, or unsigned", async () => {
  const dir = await Deno.makeTempDir();
  try {
    // Missing file.
    assertEquals(await readPlantedManifest(join(dir, "nope.json")), null);
    // Malformed JSON.
    const bad = join(dir, "bad.json");
    await Deno.writeTextFile(bad, "{not json");
    assertEquals(await readPlantedManifest(bad), null);
    // Well-formed + valid shape, but signed with a throwaway key (NOT the
    // committed signing key) -> must not be trusted.
    const sk = ed.utils.randomSecretKey();
    const wrong = join(dir, "wrong.json");
    await Deno.writeTextFile(wrong, JSON.stringify(await sign("1.0.0", sk)));
    assertEquals(await readPlantedManifest(wrong), null);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("assertManifestShape: rejects bad shapes", () => {
  assertThrows(() => assertManifestShape(null), AppError);
  assertThrows(() => assertManifestShape({ schemaVersion: 2 }), AppError);
  // missing sha256 + signature
  assertThrows(
    () =>
      assertManifestShape({
        schemaVersion: 1,
        version: "1",
        id: "x",
        tarballUrl: "u",
      }),
    AppError,
  );
});
