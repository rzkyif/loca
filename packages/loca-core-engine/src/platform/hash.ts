// Portable synchronous SHA-256, for engine services whose read-modify-write must
// not interleave across an await (the relevance staleness hash, the memory store).
// Uses @noble/hashes (pure JS, already a core dep) instead of node:crypto so the
// engine imports nothing runtime-bound. The digest is byte-identical to a
// node:crypto / crypto.subtle SHA-256, so hashes stay stable across the move.

import { sha256 } from "@noble/hashes/sha2.js";

/** Lowercase hex encoding of a byte array. */
export function toHex(bytes: Uint8Array): string {
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Synchronous SHA-256, hex-encoded lowercase. */
export function sha256HexSync(input: string | Uint8Array): string {
  const bytes = typeof input === "string" ? new TextEncoder().encode(input) : input;
  return toHex(sha256(bytes));
}
