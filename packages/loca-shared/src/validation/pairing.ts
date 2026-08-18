// Zod schemas for pairing-flow request bodies.
//
// Pairing is a PAKE (password-authenticated key exchange) handshake keyed by the
// 6-digit code, run over the capture-mode TLS connection. It replaces the old
// single unauthenticated `POST /pairing/claim`. The key confirmation folds in the
// TLS cert pin each side observes, so a man-in-the-middle that terminates TLS with
// a different cert is detected even though it relays the PAKE messages. See
// `@loca/shared/crypto/pake` for the primitive and AGENTS.md for the flow.

import { z } from "zod";

// Base64 of an N-byte buffer (no padding tolerance needed since both ends use btoa).
const base64 = (label: string) =>
  z
    .string()
    .min(1)
    .max(512)
    .regex(/^[A-Za-z0-9+/]+=*$/, {
      message: `${label} must be base64`,
    });

const clientName = z.string().min(1).max(64);

// Minimum admin-password length. The password is a second factor gated behind a
// valid bearer token (never a sole barrier on the open network), so a modest
// floor plus argon2id + rate limiting is enough. Shared so the set-password
// endpoint, the client install form, and the password modal all agree.
export const MIN_ADMIN_PASSWORD_LENGTH = 8;
export const MAX_ADMIN_PASSWORD_LENGTH = 256;

const adminPassword = z.string().min(MIN_ADMIN_PASSWORD_LENGTH).max(MAX_ADMIN_PASSWORD_LENGTH);

export const pairingCodeRequestSchema = z
  .object({
    // Optional caller-requested TTL in seconds. Core enforces a 60-min ceiling.
    ttlSec: z
      .number()
      .int()
      .min(60)
      .max(60 * 60)
      .optional(),
    // Admin password for the bearer+password mint path. Absent when the caller
    // authenticates with X-Admin-Token instead. Length is verified by argon2id,
    // not here, so we accept any non-empty string and let the core reject it.
    password: z.string().min(1).max(MAX_ADMIN_PASSWORD_LENGTH).optional(),
  })
  .strict();

export type PairingCodeRequest = z.infer<typeof pairingCodeRequestSchema>;

// Body for cross-client revoke (DELETE /pairing/clients/:id) when the caller
// authorizes with the admin password instead of X-Admin-Token. Self-revoke
// ("leave") needs neither, so the body is optional/empty there.
export const clientRevokeRequestSchema = z
  .object({
    password: z.string().min(1).max(MAX_ADMIN_PASSWORD_LENGTH).optional(),
  })
  .strict();

export type ClientRevokeRequest = z.infer<typeof clientRevokeRequestSchema>;

// Body for POST /admin/password (set/replace the admin password). Guarded by
// X-Admin-Token (device access); enforces the length floor here so a too-short
// password is rejected before hashing.
export const adminPasswordSetRequestSchema = z
  .object({
    password: adminPassword,
  })
  .strict();

export type AdminPasswordSetRequest = z.infer<typeof adminPasswordSetRequestSchema>;

// --- PAKE handshake -------------------------------------------------------

// Step 1: client (initiator) → core. `sid` is the CPace session id the client
// generated; `msgA` is the client's CPace public element (Ya).
export const pakeStartRequestSchema = z
  .object({
    clientName,
    sid: base64("sid"),
    msgA: base64("msgA"),
  })
  .strict();

export type PakeStartRequest = z.infer<typeof pakeStartRequestSchema>;

// Step 2: client → core. `confirmC` is MAC(K, "C" ‖ transcript ‖ pinClientSaw).
export const pakeFinishRequestSchema = z
  .object({
    pakeId: z.string().min(1).max(64),
    confirmC: base64("confirmC"),
  })
  .strict();

export type PakeFinishRequest = z.infer<typeof pakeFinishRequestSchema>;
