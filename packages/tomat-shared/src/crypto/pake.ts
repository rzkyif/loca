// CPace balanced PAKE, ciphersuite CPACE-RISTRETTO255-SHA512.
//
// This is the trust root of the client<->core pairing handshake: two parties
// that share a low-entropy secret (the 6-digit pairing code, used here as the
// password PRS) derive a strong shared key (ISK) that an offline dictionary
// attacker cannot recover. See `@tomat/shared/validation/pairing` for the wire
// shapes and AGENTS.md for the flow.
//
// Implemented per the CFRG draft draft-irtf-cfrg-cpace-21 (April 2026),
// Section 8.3 (G_Ristretto255) + Appendix B.3 (ristretto255 / SHA-512 test
// vectors). We run CPace in the initiator/responder mode, so the transcript is
// `transcript_ir` (Section 6.3 / 7.2). All group / map-to-curve / scalar
// arithmetic is delegated to @noble/curves' pure-JS ristretto255, so the module
// behaves identically under Deno and in a Vite browser bundle.
//
// The draft's raw ISK is the full 64-byte SHA-512 output. Both of our endpoints
// truncate it identically to 32 bytes for the public key material; the full
// 64-byte value is only reachable through the test-only `__test` seam so the
// appendix vector can be reproduced byte-for-byte.

import { ristretto255, ristretto255_hasher } from "@noble/curves/ed25519.js";
import { sha256, sha512 } from "@noble/hashes/sha2.js";
import { hmac } from "@noble/hashes/hmac.js";

// @noble/curves exposes the ristretto255 group as `ristretto255.Point`. Statics
// we use: `fromBytes` (decode, throws on bad length / invalid encoding), `ZERO`
// (identity). Instance: `multiply(scalar)`, `toBytes`, `is0()`. The
// element-derivation / one-way map taking a 64-byte uniform string is
// `ristretto255_hasher.deriveToCurve`.
const Point = ristretto255.Point;
type Pt = InstanceType<typeof Point>;

// --- ciphersuite constants ------------------------------------------------

const utf8 = (s: string): Uint8Array => new TextEncoder().encode(s);

// G_Ristretto255.DSI and G_Ristretto255.DSI_ISK (draft Section 8.3 / B.3).
const DSI = utf8("CPaceRistretto255");
const DSI_ISK = utf8("CPaceRistretto255_ISK");

// SHA-512 input block size, used to size the generator-string zero padding.
const HASH_BLOCK_BYTES = 128;
// Ristretto255 field size; calculate_generator hashes to 2 * field size.
const FIELD_SIZE_BYTES = 32;
// sample_scalar samples this many bytes then clears bits above group_size_bits.
const GROUP_SIZE_BYTES = 32;
const GROUP_SIZE_BITS = 252;

// --- string encoding helpers (draft Section 4 / 6) ------------------------

/** prepend_len: LEB128 length prefix followed by the data (draft Section 4). */
function prependLen(data: Uint8Array): Uint8Array {
  const prefix: number[] = [];
  let length = data.length;
  // LEB128, little-endian base-128.
  for (;;) {
    if (length < 0x80) {
      prefix.push(length);
      break;
    }
    prefix.push((length & 0x7f) | 0x80);
    length >>>= 7;
  }
  return concat(Uint8Array.from(prefix), data);
}

/** lv_cat: concatenation of each argument under prepend_len (draft Section 4). */
function lvCat(...args: Uint8Array[]): Uint8Array {
  return concat(...args.map(prependLen));
}

function concat(...parts: Uint8Array[]): Uint8Array {
  let total = 0;
  for (const p of parts) total += p.length;
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of parts) {
    out.set(p, off);
    off += p.length;
  }
  return out;
}

// --- generator (draft Section 8.1 / 8.3) ----------------------------------

/**
 * generator_string(DSI, PRS, CI, sid, s_in_bytes) =
 *   lv_cat(DSI, PRS, zero_bytes(len_zpad), CI, sid)
 * with len_zpad = MAX(0, s_in_bytes - len(prepend_len(PRS)) -
 *   len(prepend_len(DSI)) - 1). The zero padding fills the first hash block so
 * the byte count to hash is independent of short-password length.
 */
function generatorString(prs: Uint8Array, ci: Uint8Array, sid: Uint8Array): Uint8Array {
  const lenZpad = Math.max(
    0,
    HASH_BLOCK_BYTES - prependLen(prs).length - prependLen(DSI).length - 1,
  );
  return lvCat(DSI, prs, new Uint8Array(lenZpad), ci, sid);
}

/**
 * calculate_generator: hash the generator string to 2*field_size bytes with
 * SHA-512, then apply ristretto255 element_derivation (deriveToCurve). Returns
 * the decoded internal point _g.
 */
function calculateGenerator(prs: Uint8Array, ci: Uint8Array, sid: Uint8Array): Pt {
  const genStr = generatorString(prs, ci, sid);
  const genHash = sha512(genStr).subarray(0, 2 * FIELD_SIZE_BYTES);
  return ristretto255_hasher.deriveToCurve!(genHash);
}

// --- scalars (draft Section 8.3) ------------------------------------------

/**
 * sample_scalar (recommended form): random group_size_bytes, clear the bits
 * above group_size_bits, interpret little-endian. Returns the integer.
 */
function sampleScalar(): bigint {
  const bytes = new Uint8Array(GROUP_SIZE_BYTES);
  crypto.getRandomValues(bytes);
  return clampScalarLe(bytes);
}

/** Clear the most significant bits beyond group_size_bits, decode LE. */
function clampScalarLe(bytes: Uint8Array): bigint {
  const b = bytes.slice(0, GROUP_SIZE_BYTES);
  // group_size_bits = 252 → keep the low 4 bits of the top byte (index 31).
  const topBitsToClear = GROUP_SIZE_BYTES * 8 - GROUP_SIZE_BITS; // 4
  if (topBitsToClear > 0) {
    const mask = 0xff >> topBitsToClear;
    b[GROUP_SIZE_BYTES - 1] &= mask;
  }
  let x = 0n;
  for (let i = b.length - 1; i >= 0; i--) x = (x << 8n) | BigInt(b[i]);
  return x;
}

/**
 * scalar_mult_vfy(y, X): decode the encoded point X; if decoding fails or the
 * result is the identity, abort by throwing. Otherwise return encode(y * X).
 * The draft returns G.I on failure and the caller MUST abort on G.I; we collapse
 * that into a thrown error so callers can't forget the check.
 */
function scalarMultVfy(y: bigint, encodedX: Uint8Array): Uint8Array {
  let X: Pt;
  try {
    X = Point.fromBytes(encodedX);
  } catch {
    throw new CpaceError("invalid peer element: decode failed");
  }
  // Reject the identity input directly (low-order / neutral element).
  if (X.is0()) throw new CpaceError("invalid peer element: identity");
  const K = X.multiply(y);
  if (K.is0()) throw new CpaceError("invalid peer element: yields identity");
  return K.toBytes();
}

// --- transcript + ISK (draft Section 6.3 / 7.2) ---------------------------

/** transcript_ir(Ya,ADa,Yb,ADb) = lv_cat(Ya,ADa) || lv_cat(Yb,ADb). */
function transcriptIr(
  ya: Uint8Array,
  ada: Uint8Array,
  yb: Uint8Array,
  adb: Uint8Array,
): Uint8Array {
  return concat(lvCat(ya, ada), lvCat(yb, adb));
}

/**
 * ISK = SHA-512( lv_cat(DSI_ISK, sid, K) || transcript_ir(Ya,ADa,Yb,ADb) ).
 * Returns the full 64-byte SHA-512 output (the draft's raw ISK).
 */
function computeIsk(
  sid: Uint8Array,
  k: Uint8Array,
  ya: Uint8Array,
  ada: Uint8Array,
  yb: Uint8Array,
  adb: Uint8Array,
): Uint8Array {
  const input = concat(lvCat(DSI_ISK, sid, k), transcriptIr(ya, ada, yb, adb));
  return sha512(input);
}

/** First 32 bytes of the raw ISK, the public 256-bit key material. */
function deriveKey(rawIsk: Uint8Array): Uint8Array {
  return rawIsk.slice(0, 32);
}

class CpaceError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "CpaceError";
  }
}

// In the public flow we use no channel identifier and no associated data; the
// pairing code itself is the password and the cert-pin binding happens in the
// confirmation MAC below.
const EMPTY = new Uint8Array(0);

// --- public API -----------------------------------------------------------

/** 16 random bytes for the CPace session id, from the platform CSPRNG. */
export function randomSid(): Uint8Array {
  const sid = new Uint8Array(16);
  crypto.getRandomValues(sid);
  return sid;
}

export interface CpaceInitiator {
  /** The CPace session id this run is bound to. */
  readonly sid: Uint8Array;
  /** Message A (Ya), the initiator's public element, 32 bytes. */
  readonly msgA: Uint8Array;
  /**
   * Consume the responder's message B (Yb) and derive the shared key (32
   * bytes). Throws if msgB is an invalid / identity / wrong-length encoding.
   */
  finish(msgB: Uint8Array): Uint8Array;
}

/**
 * Begin a CPace run as the initiator (party A). Returns msgA to send to the
 * responder and a `finish` closure that completes the run on msgB. The secret
 * scalar `ya` never leaves this closure.
 *
 * `ci` is the CPace channel identifier: a public value both parties must agree
 * on for the run to succeed (it is folded into the generator, so the derived key
 * differs if the two sides disagree). In Tomat we pass the observed TLS cert
 * pin, binding it INTO the key. A MITM that presents a different cert yields a
 * different key on each side, so the handshake fails.
 */
export function cpaceInitiatorStart(
  password: string,
  sid: Uint8Array,
  ci: Uint8Array = EMPTY,
): CpaceInitiator {
  return cpaceInitiatorStartInternal(password, sid, ci, EMPTY, EMPTY, () => sampleScalar());
}

function cpaceInitiatorStartInternal(
  password: string,
  sid: Uint8Array,
  ci: Uint8Array,
  ada: Uint8Array,
  adb: Uint8Array,
  scalarSource: () => bigint,
): CpaceInitiator {
  const prs = utf8(password);
  const g = calculateGenerator(prs, ci, sid);
  const ya = scalarSource();
  const msgA = g.multiply(ya).toBytes();
  let used = false;
  return {
    sid,
    msgA,
    finish(msgB: Uint8Array): Uint8Array {
      if (used) throw new CpaceError("CPace initiator already finished");
      used = true;
      const k = scalarMultVfy(ya, msgB);
      const rawIsk = computeIsk(sid, k, msgA, ada, msgB, adb);
      return deriveKey(rawIsk);
    },
  };
}

export interface CpaceResponderResult {
  /** Message B (Yb), the responder's public element, 32 bytes. */
  readonly msgB: Uint8Array;
  /** The derived shared key, 32 bytes. */
  readonly isk: Uint8Array;
}

/**
 * Run CPace as the responder (party B): consume msgA (Ya) and the shared
 * password, producing msgB to send back plus the derived key. Throws if msgA is
 * an invalid / identity / wrong-length encoding. `ci` is the channel identifier
 * (see `cpaceInitiatorStart`); pass the server's real cert pin.
 */
export function cpaceResponder(
  password: string,
  sid: Uint8Array,
  msgA: Uint8Array,
  ci: Uint8Array = EMPTY,
): CpaceResponderResult {
  return cpaceResponderInternal(password, sid, msgA, ci, EMPTY, EMPTY, () => sampleScalar());
}

function cpaceResponderInternal(
  password: string,
  sid: Uint8Array,
  msgA: Uint8Array,
  ci: Uint8Array,
  ada: Uint8Array,
  adb: Uint8Array,
  scalarSource: () => bigint,
): CpaceResponderResult {
  const prs = utf8(password);
  const g = calculateGenerator(prs, ci, sid);
  const yb = scalarSource();
  const msgB = g.multiply(yb).toBytes();
  // Verify the peer element and derive K before exposing anything.
  const k = scalarMultVfy(yb, msgA);
  const rawIsk = computeIsk(sid, k, msgA, ada, msgB, adb);
  return { msgB, isk: deriveKey(rawIsk) };
}

// --- channel-bound key confirmation ---------------------------------------

const ROLE_BYTE: Record<"C" | "S", number> = { C: 0x43, S: 0x53 };

/** 2-byte little-endian length prefix + value, to keep fields unambiguous. */
function lv2(data: Uint8Array): Uint8Array {
  const len = data.length;
  if (len > 0xffff) throw new CpaceError("confirm field too long");
  return concat(Uint8Array.from([len & 0xff, (len >> 8) & 0xff]), data);
}

/**
 * Channel-bound confirmation tag:
 *   HMAC-SHA256(isk, role || lv(msgA) || lv(msgB) || lv(utf8(pin)))
 * `role` is the single byte 0x43 ("C") or 0x53 ("S"). `pin` is the value each
 * side observed for the channel (e.g. the TLS cert pin); folding it in lets a
 * MITM that re-terminates TLS be detected even though it relays the PAKE
 * messages. Returns the 32-byte HMAC.
 */
export function confirmTag(
  isk: Uint8Array,
  role: "C" | "S",
  msgA: Uint8Array,
  msgB: Uint8Array,
  pin: string,
): Uint8Array {
  const msg = concat(Uint8Array.from([ROLE_BYTE[role]]), lv2(msgA), lv2(msgB), lv2(utf8(pin)));
  return hmac(sha256, isk, msg);
}

/** Constant-time verify of a confirmation tag against the recomputed value. */
export function verifyConfirm(
  tag: Uint8Array,
  isk: Uint8Array,
  role: "C" | "S",
  msgA: Uint8Array,
  msgB: Uint8Array,
  pin: string,
): boolean {
  const expected = confirmTag(isk, role, msgA, msgB, pin);
  return constantTimeEqual(tag, expected);
}

function constantTimeEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}

// --- test-only seam -------------------------------------------------------
//
// Exposes internals so `pake.test.ts` can drive the protocol with the fixed
// scalars, CI, and associated data from a published test vector and check the
// raw 64-byte ISK. NOT part of the supported API; do not import outside tests.
export const __test: {
  calculateGenerator: typeof calculateGenerator;
  scalarMultVfy: typeof scalarMultVfy;
  clampScalarLe: typeof clampScalarLe;
  computeIsk: typeof computeIsk;
  transcriptIr: typeof transcriptIr;
  generatorString: typeof generatorString;
  lvCat: typeof lvCat;
  initiatorStart: typeof cpaceInitiatorStartInternal;
  responder: typeof cpaceResponderInternal;
  pointToBytes: (scalarLe: Uint8Array, g: Pt) => Uint8Array;
} = {
  calculateGenerator,
  scalarMultVfy,
  clampScalarLe,
  computeIsk,
  transcriptIr,
  generatorString,
  lvCat,
  initiatorStart: cpaceInitiatorStartInternal,
  responder: cpaceResponderInternal,
  pointToBytes: (scalarLe, g) => g.multiply(clampScalarLe(scalarLe)).toBytes(),
};
