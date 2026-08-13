// Binary-manifest signature + shape verification. These are the controls that
// keep a tampered/MITM'd binaries manifest from installing attacker-chosen
// binaries, yet had zero coverage. The signature check is exercised with a
// throwaway keypair via the key-injectable `verifyBinariesSignature`; the
// production path passes the embedded signing key. The signature covers the
// whole manifest minus `signature`, so the monotonic `version` is authenticated
// alongside `binaries`.

import { assertEquals, assertThrows } from "@std/assert";
import * as ed from "@noble/ed25519";
import { encodeBase64 } from "@std/encoding/base64";
import { canonicalize } from "@loca/shared";
import type { BinaryManifest } from "@loca/shared";
import { assertManifestShape, verifyBinariesSignature } from "./manifest.ts";
import { AppError } from "@loca/core-engine";

const SAMPLE_MANIFEST = {
  schemaVersion: 1,
  version: "1.2.3",
  binaries: {
    "llama-server": {
      version: "1.2.3",
      platforms: {
        "aarch64-apple-darwin": { url: "https://r2/x", sha256: "abc" },
      },
    },
  },
} as unknown as BinaryManifest;

async function sign(
  manifest: Omit<BinaryManifest, "signature">,
  priv: Uint8Array,
): Promise<BinaryManifest> {
  const sig = await ed.signAsync(new TextEncoder().encode(canonicalize(manifest)), priv);
  return { ...manifest, signature: encodeBase64(sig) };
}

Deno.test("verifyBinariesSignature: accepts a correctly signed manifest", async () => {
  const priv = ed.utils.randomSecretKey();
  const pub = await ed.getPublicKeyAsync(priv);
  const signed = await sign(SAMPLE_MANIFEST, priv);
  assertEquals(await verifyBinariesSignature(signed, pub), true);
});

Deno.test("verifyBinariesSignature: rejects a tampered binaries body", async () => {
  const priv = ed.utils.randomSecretKey();
  const pub = await ed.getPublicKeyAsync(priv);
  const signed = await sign(SAMPLE_MANIFEST, priv);
  // Attacker swaps the download URL after signing.
  const tampered: BinaryManifest = {
    ...signed,
    binaries: {
      "llama-server": {
        version: "1.2.3",
        platforms: {
          "aarch64-apple-darwin": { url: "https://evil/x", sha256: "abc" },
        },
      },
    } as unknown as BinaryManifest["binaries"],
  };
  assertEquals(await verifyBinariesSignature(tampered, pub), false);
});

Deno.test("verifyBinariesSignature: rejects a downgraded version after signing", async () => {
  const priv = ed.utils.randomSecretKey();
  const pub = await ed.getPublicKeyAsync(priv);
  const signed = await sign(SAMPLE_MANIFEST, priv);
  // Attacker tries to relabel a v1.2.3 manifest as an older v1.0.0 to slip it
  // past the downgrade guard: the version is signed, so verification fails.
  const relabeled: BinaryManifest = { ...signed, version: "1.0.0" };
  assertEquals(await verifyBinariesSignature(relabeled, pub), false);
});

Deno.test("verifyBinariesSignature: rejects a signature made with a different key", async () => {
  const priv = ed.utils.randomSecretKey();
  const otherPub = await ed.getPublicKeyAsync(ed.utils.randomSecretKey());
  const signed = await sign(SAMPLE_MANIFEST, priv);
  assertEquals(await verifyBinariesSignature(signed, otherPub), false);
});

Deno.test("assertManifestShape: accepts a valid manifest", () => {
  assertManifestShape({
    schemaVersion: 1,
    version: "1.0.0",
    binaries: {},
    signature: "abc",
  });
});

Deno.test("assertManifestShape: rejects a missing version", () => {
  assertThrows(
    () => assertManifestShape({ schemaVersion: 1, binaries: {}, signature: "x" }),
    AppError,
    "missing version",
  );
});

Deno.test("assertManifestShape: rejects a missing/empty signature", () => {
  assertThrows(
    () => assertManifestShape({ schemaVersion: 1, version: "1.0.0", binaries: {} }),
    AppError,
    "missing signature",
  );
  assertThrows(
    () =>
      assertManifestShape({
        schemaVersion: 1,
        version: "1.0.0",
        binaries: {},
        signature: "",
      }),
    AppError,
    "missing signature",
  );
});

Deno.test("assertManifestShape: rejects a wrong schemaVersion / missing binaries / non-object", () => {
  assertThrows(
    () =>
      assertManifestShape({
        schemaVersion: 2,
        version: "1.0.0",
        binaries: {},
        signature: "x",
      }),
    AppError,
    "schemaVersion",
  );
  assertThrows(
    () =>
      assertManifestShape({
        schemaVersion: 1,
        version: "1.0.0",
        signature: "x",
      }),
    AppError,
    "missing binaries",
  );
  assertThrows(() => assertManifestShape(null), AppError, "not an object");
});
