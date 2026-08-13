// Signature-critical serialization shared by the release signer and every
// verifier. The signer (scripts/release) canonicalizes a manifest body and
// Ed25519-signs the bytes; the core binary-manifest verifier and the core
// self-update verifier each recompute the identical bytes to check the
// signature. These were copied into three places and HAD to agree byte-for-byte
// or signatures would silently fail to verify; this is now the single source.

/** Deterministic JSON: object keys sorted lexicographically, arrays left in
 *  order, no insignificant whitespace. */
export function canonicalize(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) {
    return "[" + value.map(canonicalize).join(",") + "]";
  }
  const obj = value as Record<string, unknown>;
  const keys = Object.keys(obj).sort();
  return "{" + keys.map((k) => JSON.stringify(k) + ":" + canonicalize(obj[k])).join(",") + "}";
}

/** Decode a standard-alphabet base64 string (Ed25519 public keys + signatures)
 *  to bytes. */
export function decodeBase64(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
