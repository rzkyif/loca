// TLS service: self-signed cert generation + the SPKI pin. The key invariant is
// that the pin is stable across cert regeneration (and bindHost/SAN changes)
// because it is computed over the persisted key's SubjectPublicKeyInfo.

// Loads the Reflect polyfill @peculiar/x509 needs (see tls.ts); must stay above
// the direct x509 import below since this test is its own entry point.
import "reflect-metadata";
import { assertEquals } from "@std/assert";
import { encodeBase64 } from "@std/encoding/base64";
import * as x509 from "@peculiar/x509";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { __resetForTesting, tlsCertFingerprint, tlsServeOptions } from "./tls.ts";

x509.cryptoProvider.set(crypto);

function sanValues(certPem: string): string[] {
  const cert = new x509.X509Certificate(certPem);
  const san = cert.getExtension(x509.SubjectAlternativeNameExtension);
  return san ? san.names.items.map((n) => n.value) : [];
}

Deno.test("tls: serve options are valid PEM cert + key", async () => {
  const env = await setupTestEnv();
  try {
    const { cert, key } = await tlsServeOptions("127.0.0.1");
    assertEquals(cert.includes("BEGIN CERTIFICATE"), true);
    assertEquals(key.includes("BEGIN PRIVATE KEY"), true);
  } finally {
    await env.teardown();
  }
});

Deno.test("tls: cert SANs cover loopback + the configured bind host", async () => {
  const env = await setupTestEnv();
  try {
    const { cert } = await tlsServeOptions("192.168.1.50");
    const sans = sanValues(cert);
    for (const want of ["127.0.0.1", "localhost", "::1", "192.168.1.50"]) {
      assertEquals(sans.includes(want), true, `SAN missing ${want}: ${sans}`);
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("tls: pin == base64(SHA-256(SPKI)) of the cert's own key (Rust-verifier parity)", async () => {
  const env = await setupTestEnv();
  try {
    const { cert } = await tlsServeOptions("127.0.0.1");
    const pin = await tlsCertFingerprint();
    // Derive the pin independently from the cert's embedded SubjectPublicKeyInfo
    // exactly as the client's Rust SpkiPinVerifier does over the presented cert
    // (sha256 of x509 public_key().raw, base64-standard). If either side's
    // derivation drifts, pairing's key confirmation silently fails; this locks
    // the core half of that contract.
    const spki = new x509.X509Certificate(cert).publicKey.rawData;
    const expected = encodeBase64(new Uint8Array(await crypto.subtle.digest("SHA-256", spki)));
    assertEquals(pin, expected);
    // Standard padded base64 of a 32-byte digest.
    assertEquals(atob(pin).length, 32);
  } finally {
    await env.teardown();
  }
});

Deno.test("tls: fingerprint is base64 SHA-256 and stable across cert regen", async () => {
  const env = await setupTestEnv();
  try {
    await tlsServeOptions("127.0.0.1");
    const pin1 = await tlsCertFingerprint();
    // base64 of a 32-byte SHA-256 digest → 44 chars.
    assertEquals(pin1.length, 44);

    // Regenerate the cert with DIFFERENT SANs but the SAME sealed key (drop the
    // in-memory cache only, keep the vault). The pin must not change.
    __resetForTesting();
    const { cert: cert2 } = await tlsServeOptions("192.168.1.50");
    assertEquals(cert2.includes("BEGIN CERTIFICATE"), true);
    const pin2 = await tlsCertFingerprint();
    assertEquals(pin2, pin1);
  } finally {
    await env.teardown();
  }
});
