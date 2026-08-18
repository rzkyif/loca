// The TLS SPKI pin, asserted from the TypeScript side of the port.
//
// `packages/loca-core-rs/src/transport/testdata/tls_pin_vectors.json` is the
// fixture the Rust core's pin derivation is tested against. It was generated
// from WebCrypto, so without this test it is a Rust self-portrait: the Rust
// asserts it reproduces a file that only the Rust reads. This asserts the other
// half, that WebCrypto still derives the same pins from the same keys, which is
// what makes the fixture a genuine cross-language pin.
//
// The pin is one of the port's byte-exact parity surfaces. Every already-paired
// client pinned this value, so a derivation that drifts by one byte breaks all
// of them on upgrade, and the failure presents as "pairing stopped working"
// with nothing in the logs pointing at it.
//
// The three calls below are exactly the ones `tls.ts` makes at lines 133-136.
// Keep them inline rather than importing `build()`: the point is to derive the
// pin independently, not to confirm the module agrees with itself.

import { assertEquals } from "@std/assert";
import { encodeBase64 } from "@std/encoding/base64";
import { fromFileUrl } from "@std/path";

const VECTORS_PATH = fromFileUrl(
  import.meta.resolve("../../../loca-core-rs/src/transport/testdata/tls_pin_vectors.json"),
);

const KEY_ALG = { name: "ECDSA", namedCurve: "P-256" } as const;

interface Vector {
  note: string;
  jwk: JsonWebKey;
  pin: string;
}

/** base64(SHA-256(SPKI DER)), from a private JWK. */
async function pinFor(jwk: JsonWebKey): Promise<string> {
  // WebCrypto cannot derive a public key from an imported EC private key, so
  // the public half is reconstructed from the same JWK with `d` stripped, the
  // way `keyPairFromPrivateJwk` does it.
  const { d: _d, ...publicJwk } = jwk;
  const publicKey = await crypto.subtle.importKey(
    "jwk",
    { ...publicJwk, key_ops: ["verify"] },
    KEY_ALG,
    true,
    ["verify"],
  );
  const spki = await crypto.subtle.exportKey("spki", publicKey);
  const digest = await crypto.subtle.digest("SHA-256", spki);
  return encodeBase64(new Uint8Array(digest));
}

Deno.test("TLS pin: WebCrypto derives the pins the Rust core is tested against", async () => {
  const { vectors } = JSON.parse(await Deno.readTextFile(VECTORS_PATH)) as {
    vectors: Vector[];
  };
  assertEquals(vectors.length > 0, true, "the fixture must not be empty");

  for (const v of vectors) {
    assertEquals(await pinFor(v.jwk), v.pin, `pin drifted for: ${v.note}`);
  }
});

Deno.test("TLS pin: the fixture still covers every leading-zero position", async () => {
  // A field whose first byte is zero is where an implementation that routes it
  // through a big integer drops a byte, producing a plausible, stable, WRONG
  // pin that every ordinary test passes. `d`, `x` and `y` sit at different
  // offsets, so all three need their own vector: an earlier fixture covered
  // "x or y" with one key and left the other offset untested.
  const { vectors } = JSON.parse(await Deno.readTextFile(VECTORS_PATH)) as {
    vectors: Vector[];
  };
  const firstByte = (b64url: string): number => {
    const b64 = b64url.replaceAll("-", "+").replaceAll("_", "/");
    return atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4)).charCodeAt(0);
  };

  for (const field of ["d", "x", "y"] as const) {
    const covered = vectors.some((v) => firstByte(v.jwk[field] as string) === 0);
    assertEquals(covered, true, `no vector has a leading zero byte in \`${field}\``);
  }
});
