// Detached Ed25519 verification, shared by any client that authenticates a
// release manifest the way core does. The Android self-host updater uses it to
// verify android.json (and the APK sha256 it carries) against the committed
// signing public key before downloading, mirroring core's manifest verifiers.

import { ed25519 } from "@noble/curves/ed25519.js";
import { decodeBase64 } from "./canonical.ts";

/** Verify a detached Ed25519 signature (standard-base64) over the exact
 *  `message` bytes against a base64 public key. Returns false on any malformed
 *  input rather than throwing, so callers can treat verification as a boolean
 *  gate. */
export function verifyEd25519Detached(
  publicKeyB64: string,
  signatureB64: string,
  message: Uint8Array,
): boolean {
  try {
    return ed25519.verify(decodeBase64(signatureB64), message, decodeBase64(publicKeyB64));
  } catch {
    return false;
  }
}

/** Lowercase-hex SHA-256 of `bytes` via Web Crypto (available in every WebView
 *  and Deno), matching the `sha256File` hex digest the release pipeline embeds
 *  in android.json. */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes as unknown as ArrayBuffer);
  const out = new Uint8Array(digest);
  let hex = "";
  for (let i = 0; i < out.length; i++) hex += out[i].toString(16).padStart(2, "0");
  return hex;
}

/** Constant-time-ish equality for two hex digests of equal expected length. Not
 *  a defense against a determined timing attacker (JS strings leak length), but
 *  it avoids the obvious early-return short-circuit when comparing a hash. */
export function timingSafeEqualHex(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}
