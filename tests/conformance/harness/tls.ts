// SPKI pin derivation, done independently of whatever the core claims.
//
// The pin is base64(SHA-256(SubjectPublicKeyInfo DER)) (services/tls.ts:133-136).
// Pairing folds it into the CPace channel identifier AND the key confirmation,
// so a harness that derived it wrongly could not pair at all: the value is
// self-checking. Deriving it from the cert the core wrote, rather than asking
// the core for it, is what makes it a cross-check rather than an echo.

// @peculiar/x509 wires its providers through tsyringe, which needs the Reflect
// metadata polyfill in scope before x509 evaluates. Must stay above the import.
import "reflect-metadata";
import * as x509 from "@peculiar/x509";
import { encodeBase64 } from "@std/encoding/base64";
import { join } from "@std/path";

x509.cryptoProvider.set(crypto);

/** The public cert the core drops beside its root at boot (main.ts:269,
 *  paths.ts:194), so a co-located admin tool can trust the loopback API. */
export function certFile(home: string): string {
  return join(home, "tls-cert.pem");
}

/** base64(SHA-256(SPKI)) from a PEM certificate. */
export async function pinFromPem(pem: string): Promise<string> {
  const cert = new x509.X509Certificate(pem);
  const digest = await crypto.subtle.digest("SHA-256", cert.publicKey.rawData);
  return encodeBase64(new Uint8Array(digest));
}

/** Derive the pin from the core's own `tls-cert.pem`, polling until it appears.
 *  The file is written with a detached promise during boot, so it can lag the
 *  first healthy response by a few milliseconds. */
export async function pinFromHome(home: string, timeoutMs = 5_000): Promise<string> {
  const path = certFile(home);
  const deadline = Date.now() + timeoutMs;
  let lastErr: unknown;
  while (Date.now() < deadline) {
    try {
      return await pinFromPem(await Deno.readTextFile(path));
    } catch (err) {
      lastErr = err;
      await new Promise((r) => setTimeout(r, 50));
    }
  }
  throw new Error(
    `could not derive the TLS pin from ${path}: ${lastErr}. The core must write ` +
      `its public cert beside its root at boot; without it the pairing PAKE has ` +
      `nothing to channel-bind.`,
  );
}
