// Admin-password hashing for the remote-pairing flow.
//
// The admin password lets an already-paired client mint pairing codes / revoke
// other devices without reading the on-disk admin token (see
// services/auth.ts and AGENTS.md). Because it is a memorable, low-entropy
// secret, we hash it with argon2id (per-install random salt, OWASP-floor
// parameters) so a leaked hash file resists offline cracking and each online
// guess costs real work. The verify path in auth.ts rate-limits BEFORE calling
// here so argon2id can't be turned into a CPU DoS.
//
// On-disk format is a single self-describing line, PHC-flavored but minimal:
//   argon2id$m=<KiB>,t=<iters>,p=<lanes>$<saltB64>$<hashB64>

import { argon2id } from "@noble/hashes/argon2.js";
import { decodeBase64, encodeBase64 } from "@std/encoding/base64";

// OWASP minimum for argon2id (second-choice profile): 19 MiB, 2 iterations,
// 1 lane. Cheap enough for an interactive set/verify, expensive enough that a
// gated, rate-limited online attacker makes no headway.
const ARGON_M_KIB = 19 * 1024;
const ARGON_T = 2;
const ARGON_P = 1;
const HASH_LEN = 32;
const SALT_LEN = 16;

function derive(password: string, salt: Uint8Array, m: number, t: number, p: number): Uint8Array {
  return argon2id(new TextEncoder().encode(password), salt, { m, t, p, dkLen: HASH_LEN });
}

/** Hash a password into the on-disk format string. Generates a fresh salt. */
export function hashAdminPassword(password: string): string {
  const salt = crypto.getRandomValues(new Uint8Array(SALT_LEN));
  const hash = derive(password, salt, ARGON_M_KIB, ARGON_T, ARGON_P);
  return `argon2id$m=${ARGON_M_KIB},t=${ARGON_T},p=${ARGON_P}$${encodeBase64(salt)}$${encodeBase64(
    hash,
  )}`;
}

/** Verify a password against a stored format string. Returns false (never
 *  throws) on any parse error or mismatch. Constant-time over the hash bytes. */
export function verifyAdminPassword(password: string, stored: string): boolean {
  const parts = stored.trim().split("$");
  if (parts.length !== 4 || parts[0] !== "argon2id") return false;
  const params = parseParams(parts[1]);
  if (!params) return false;
  let salt: Uint8Array;
  let expected: Uint8Array;
  try {
    salt = decodeBase64(parts[2]);
    expected = decodeBase64(parts[3]);
  } catch {
    return false;
  }
  if (expected.length !== HASH_LEN) return false;
  const actual = derive(password, salt, params.m, params.t, params.p);
  return constantTimeEqual(actual, expected);
}

function parseParams(s: string): { m: number; t: number; p: number } | null {
  const out: Record<string, number> = {};
  for (const kv of s.split(",")) {
    const [k, v] = kv.split("=");
    const n = Number(v);
    if (!k || !Number.isInteger(n) || n <= 0) return null;
    out[k] = n;
  }
  if (out.m === undefined || out.t === undefined || out.p === undefined) return null;
  return { m: out.m, t: out.t, p: out.p };
}

function constantTimeEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}
