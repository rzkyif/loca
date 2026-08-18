// Response shapes for the pairing routes.
//
// These live here rather than beside the request validators in
// `../validation/pairing.ts` because nothing validates an outbound body: they
// are contract, not validation. Keeping them here is also what lets the Rust
// core generate them, since the generator that covers the plain-TypeScript half
// of the contract cannot walk a module full of `z.infer<typeof ...>` aliases.
// The alternative was the core hand-writing them, which is a second source of
// truth for a wire shape.

export interface PairingCodeResponse {
  code: string;
  expiresAtMs: number;
}

// --- PAKE handshake -------------------------------------------------------

export interface PakeStartResponse {
  // Opaque handle for the in-flight handshake; echoed back in the finish call.
  pakeId: string;
  // Core's CPace public element (Yb).
  msgB: string;
}

export interface PakeFinishResponse {
  token: string;
  clientId: string;
  coreVersion: string;
  // MAC(K, "S" concat transcript concat pinCoreHas). The client verifies it
  // against the pin it observed, authenticating core and confirming the pin
  // (no MITM).
  confirmS: string;
}

export interface PairedClientEntry {
  id: string;
  name: string;
  createdAtMs: number;
  lastSeenMs: number;
  isMe: boolean;
}
