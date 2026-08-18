// Generates the TLS SPKI pin vectors the Rust port is tested against.
//
//   deno run --allow-write=. scripts/parity/tls-pin-vectors.ts \
//     packages/loca-core-rs/src/transport/testdata/tls_pin_vectors.json
//
// The pin derivation is one of the byte-exact parity surfaces: every
// already-paired client pinned this value at pairing time, so the Rust core must
// reproduce it for the same key or every client breaks on upgrade with no
// obvious diagnosis.
//
// This file is the ORACLE. It reproduces packages/loca-core/src/services/tls.ts
// lines 133-136 with nothing in between:
//
//     const spki = await crypto.subtle.exportKey("spki", publicKey);
//     const digest = await crypto.subtle.digest("SHA-256", spki);
//     const fingerprint = encodeBase64(new Uint8Array(digest));
//
// The public key is reconstructed from the private JWK with `d` stripped,
// exactly as keyPairFromPrivateJwk does (tls.ts:70-81), because WebCrypto
// cannot derive a public key from an imported EC private key.
//
// Vectors are generated once and committed. Regenerating produces DIFFERENT
// keys and therefore different pins, which is fine (nothing outside the test
// depends on these particular keys) but pointless: the value of the fixture is
// that it was produced by this implementation and not by the one under test.

import { encodeBase64 } from "@std/encoding/base64";

const KEY_ALG = { name: "ECDSA", namedCurve: "P-256" } as const;

interface Vector {
  note: string;
  jwk: JsonWebKey;
  pin: string;
}

/** base64(SHA-256(SPKI DER)), derived from a private JWK. */
async function pinFor(jwk: JsonWebKey): Promise<string> {
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

function decode(b64url: string): Uint8Array {
  const b64 = b64url.replaceAll("-", "+").replaceAll("_", "/");
  const bin = atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4));
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

/** Whether the field's first byte is actually zero.
 *
 * Decoded rather than sniffed from the base64: a leading "A" only means the top
 * six bits are clear, so it matches any first byte from 0 to 3 and is true for
 * roughly 1 key in 64 regardless of whether a zero is present. Checking the
 * bytes is the difference between a fixture that covers this case and one that
 * merely claims to. Found by mutation testing: with the sniffing version, an
 * implementation that trimmed a leading zero coordinate passed. */
function leadingZero(b64url: string): boolean {
  return decode(b64url)[0] === 0;
}

/** Keep generating until the predicate accepts the key, so the vector set
 *  covers the encodings that break a big-integer-based implementation. */
async function generateUntil(accept: (jwk: JsonWebKey) => boolean): Promise<JsonWebKey> {
  for (let i = 0; i < 20000; i++) {
    const kp = (await crypto.subtle.generateKey(KEY_ALG, true, [
      "sign",
      "verify",
    ])) as CryptoKeyPair;
    const jwk = await crypto.subtle.exportKey("jwk", kp.privateKey);
    if (accept(jwk)) return jwk;
  }
  throw new Error("no key matched the predicate in 20000 tries");
}

async function main(): Promise<void> {
  const out = Deno.args[0];
  if (!out) {
    console.error("usage: tls-pin-vectors.ts <output.json>");
    Deno.exit(2);
  }

  const cases: Array<{ note: string; accept: (jwk: JsonWebKey) => boolean }> = [
    {
      note: "ordinary key: no leading zero byte anywhere",
      accept: (j) => !leadingZero(j.d!) && !leadingZero(j.x!) && !leadingZero(j.y!),
    },
    {
      // A private scalar with a leading 0x00 is where an implementation that
      // routes `d` through a big integer loses a byte, derives a different
      // public point, and produces a wrong pin that looks entirely plausible.
      note: "private scalar d has a leading zero byte",
      accept: (j) => leadingZero(j.d!),
    },
    {
      // Same failure mode one level out: a short-encoded affine coordinate
      // shifts the uncompressed point and changes the SPKI bytes.
      //
      // x and y are SEPARATE cases on purpose. They sit at different offsets in
      // the SPKI (27 and 59), so a fixture that covers "x or y" covers whichever
      // one the generator happened to find and silently leaves the other
      // offset untested. Verified by mutation: trimming a leading zero at
      // offset 27 went unnoticed while the fixture only had a leading-zero y.
      note: "public coordinate x has a leading zero byte",
      accept: (j) => leadingZero(j.x!),
    },
    {
      note: "public coordinate y has a leading zero byte",
      accept: (j) => leadingZero(j.y!),
    },
  ];

  const vectors: Vector[] = [];
  for (const c of cases) {
    const jwk = await generateUntil(c.accept);
    vectors.push({ note: c.note, jwk, pin: await pinFor(jwk) });
  }

  const doc = {
    $comment:
      "Generated by scripts/parity/tls-pin-vectors.ts from WebCrypto, " +
      "reproducing packages/loca-core/src/services/tls.ts:133-136. Do not hand-edit: " +
      "regenerate, or the fixture stops being an independent oracle.",
    vectors,
  };
  await Deno.writeTextFile(out, JSON.stringify(doc, null, 2) + "\n");
  console.log(`wrote ${vectors.length} vectors to ${out}`);
  for (const v of vectors) console.log(`  ${v.pin}  ${v.note}`);
}

if (import.meta.main) await main();
