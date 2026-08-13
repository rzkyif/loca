// Shared SHA-256 + hex helpers. One home for the hex-encoding and SHA-256
// digest patterns that were previously copied across downloads, extensions,
// auth, tls, and the self-updater.

import { createHash } from "node:crypto";

/** Lowercase hex encoding of a byte array. */
export function toHex(bytes: Uint8Array): string {
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Synchronous SHA-256 (node:crypto), hex-encoded lowercase. For call sites
 *  whose read-modify-write must not interleave with other callers across an
 *  await (e.g. the memory store). */
export function sha256HexSync(input: string | Uint8Array): string {
  const hash = createHash("sha256");
  hash.update(input);
  return hash.digest("hex");
}

/** SHA-256 of a string or byte buffer, hex-encoded lowercase. */
export async function sha256Hex(input: string | Uint8Array): Promise<string> {
  // A passed-in Uint8Array is typed over ArrayBufferLike; digest wants a view
  // backed by a plain ArrayBuffer, so assert it (TextEncoder already returns one).
  const data =
    typeof input === "string"
      ? new TextEncoder().encode(input)
      : (input as Uint8Array<ArrayBuffer>);
  const buf = await crypto.subtle.digest("SHA-256", data);
  return toHex(new Uint8Array(buf));
}

// Incremental SHA-256 over chunks (node:crypto). Hashes each chunk as it
// arrives so a multi-GB stream (e.g. a model GGUF or the core binary during
// self-update) is verified with constant memory, instead of buffering the whole
// artifact and a merged copy before a one-shot digest. hexDigest() finalizes and
// may be called only once.
export class Sha256Stream {
  private readonly hash = createHash("sha256");
  update(chunk: Uint8Array): void {
    this.hash.update(chunk);
  }
  // Async to preserve the call-site contract; the digest itself is synchronous.
  hexDigest(): Promise<string> {
    return Promise.resolve(this.hash.digest("hex"));
  }
}
