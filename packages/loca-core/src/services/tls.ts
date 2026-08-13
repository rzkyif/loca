// TLS material for the HTTPS/WSS API.
//
// Core serves the HTTP+WS API over TLS with a SELF-SIGNED cert; clients pin it
// (no public CA exists for 127.0.0.1 / LAN IPs). The trust model:
//
//   - An ECDSA P-256 keypair is generated ONCE and sealed in the secrets vault
//     (services/secrets.ts) as a private JWK. ECDSA P-256, not Ed25519: rustls
//     and browsers support it; Ed25519 TLS certs are poorly supported.
//   - The self-signed cert is regenerated on EVERY boot from that key with the
//     current SANs (127.0.0.1, ::1, localhost, + bindHost when concrete). Cheap.
//   - The pin is `base64(SHA-256(SubjectPublicKeyInfo))`. It depends only on the
//     KEY, so it is stable across cert regen and bindHost changes (no re-pair on
//     an IP change). Clients pin this value, established + authenticated via the
//     pairing PAKE. Matches the RFC 7469 / rustls pin convention.
//
// `tlsServeOptions()` feeds `Deno.serve({ ..., cert, key })`; `tlsCertFingerprint()`
// is the pin the pairing handshake binds.

// @peculiar/x509 wires its providers through tsyringe, which needs a Reflect
// metadata polyfill in scope before x509 evaluates. This is the sole x509
// importer, so this side-effect import covers the app entry and the co-located
// tests alike. Must stay above the x509 import.
import "reflect-metadata";
import * as x509 from "@peculiar/x509";
import { encodeBase64 } from "@std/encoding/base64";
import { getLogger } from "../shared/log.ts";
import { toHex } from "../shared/hash.ts";
import { getSecret, setSecret } from "@loca/core-engine/services/secrets";

x509.cryptoProvider.set(crypto);

const log = getLogger("tls");

const KEY_SECRET = "tls-key"; // private JWK (kty/crv/x/y/d), sealed in secrets.enc
const KEY_ALG = { name: "ECDSA", namedCurve: "P-256" } as const;
const SIGN_ALG = { name: "ECDSA", hash: "SHA-256" } as const;
const CERT_VALID_YEARS = 10;

interface TlsMaterial {
  certPem: string;
  keyPem: string;
  /** base64(SHA-256(SPKI DER)). */
  fingerprint: string;
}

let cached: TlsMaterial | null = null;

// Test-only: drops cached material so the next call rebuilds it. Use between
// tests that swap LOCA_CORE_HOME.
export function __resetForTesting(): void {
  cached = null;
}

/** Load the sealed keypair, generating + sealing one on first run. */
async function loadOrCreateKeyPair(): Promise<CryptoKeyPair> {
  const existing = await getSecret(KEY_SECRET);
  if (existing) {
    const jwk = JSON.parse(existing) as JsonWebKey;
    return await keyPairFromPrivateJwk(jwk);
  }
  const kp = (await crypto.subtle.generateKey(KEY_ALG, true, ["sign", "verify"])) as CryptoKeyPair;
  const jwk = await crypto.subtle.exportKey("jwk", kp.privateKey);
  await setSecret(KEY_SECRET, JSON.stringify(jwk));
  log.info("generated new TLS keypair (ECDSA P-256), sealed in secrets vault");
  return kp;
}

// WebCrypto can't derive a public key from an imported EC private key, so we
// reconstruct the public key from the same JWK with `d` stripped.
async function keyPairFromPrivateJwk(jwk: JsonWebKey): Promise<CryptoKeyPair> {
  const privateKey = await crypto.subtle.importKey("jwk", jwk, KEY_ALG, true, ["sign"]);
  const { d: _d, ...publicJwk } = jwk;
  const publicKey = await crypto.subtle.importKey(
    "jwk",
    { ...publicJwk, key_ops: ["verify"] },
    KEY_ALG,
    true,
    ["verify"],
  );
  return { privateKey, publicKey };
}

function isIpAddress(host: string): boolean {
  // Good-enough split between IPv4/IPv6 literals and DNS names for SAN typing.
  return /^\d{1,3}(\.\d{1,3}){3}$/.test(host) || host.includes(":");
}

function sansFor(bindHost: string): x509.JsonGeneralName[] {
  const names: x509.JsonGeneralName[] = [
    { type: "dns", value: "localhost" },
    { type: "ip", value: "127.0.0.1" },
    { type: "ip", value: "::1" },
  ];
  const extra = bindHost.trim();
  const seen = new Set(names.map((n) => n.value));
  // 0.0.0.0 means "all interfaces" (not a connectable name), so skip it. With
  // certificate pinning the verifier ignores SANs anyway, so this is cosmetic.
  if (extra && extra !== "0.0.0.0" && !seen.has(extra)) {
    names.push({ type: isIpAddress(extra) ? "ip" : "dns", value: extra });
  }
  return names;
}

function toPem(der: ArrayBuffer, label: string): string {
  const b64 = encodeBase64(new Uint8Array(der));
  const wrapped = b64.match(/.{1,64}/g)?.join("\n") ?? b64;
  return `-----BEGIN ${label}-----\n${wrapped}\n-----END ${label}-----\n`;
}

async function build(bindHost: string): Promise<TlsMaterial> {
  const { privateKey, publicKey } = await loadOrCreateKeyPair();

  const now = Date.now();
  const serial = toHex(crypto.getRandomValues(new Uint8Array(16)));
  const cert = await x509.X509CertificateGenerator.createSelfSigned({
    serialNumber: serial,
    name: "CN=loca-core",
    notBefore: new Date(now - 60_000), // 1-min skew tolerance
    notAfter: new Date(now + CERT_VALID_YEARS * 365 * 24 * 60 * 60 * 1000),
    signingAlgorithm: SIGN_ALG,
    keys: { privateKey, publicKey },
    extensions: [
      new x509.BasicConstraintsExtension(false, undefined, true),
      new x509.KeyUsagesExtension(
        x509.KeyUsageFlags.digitalSignature | x509.KeyUsageFlags.keyEncipherment,
        true,
      ),
      new x509.ExtendedKeyUsageExtension(["1.3.6.1.5.5.7.3.1"]), // serverAuth
      new x509.SubjectAlternativeNameExtension(sansFor(bindHost)),
    ],
  });

  const pkcs8 = await crypto.subtle.exportKey("pkcs8", privateKey);
  const spki = await crypto.subtle.exportKey("spki", publicKey);
  const digest = await crypto.subtle.digest("SHA-256", spki);
  const fingerprint = encodeBase64(new Uint8Array(digest));

  return {
    certPem: cert.toString("pem"),
    keyPem: toPem(pkcs8, "PRIVATE KEY"),
    fingerprint,
  };
}

async function material(bindHost: string): Promise<TlsMaterial> {
  if (!cached) cached = await build(bindHost);
  return cached;
}

/** `{ cert, key }` PEM strings for `Deno.serve`. Pass the resolved bind host so
 *  the cert SANs cover it (cosmetic under pinning, but correct for the web build). */
export async function tlsServeOptions(bindHost: string): Promise<{ cert: string; key: string }> {
  const m = await material(bindHost);
  return { cert: m.certPem, key: m.keyPem };
}

/** base64(SHA-256(SPKI)): the value the pairing PAKE channel-binds and clients
 *  pin. Stable across reboots and bindHost changes. */
export async function tlsCertFingerprint(): Promise<string> {
  // bindHost only affects SANs, not the pin; resolve against whatever the cert
  // was already built with (or 127.0.0.1 if called first).
  const m = await material("127.0.0.1");
  return m.fingerprint;
}
