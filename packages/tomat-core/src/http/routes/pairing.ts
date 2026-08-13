import { Hono } from "hono";
import type { Context } from "hono";
import {
  clientRevokeRequestSchema,
  errMessage,
  pairingCodeRequestSchema,
  pakeFinishRequestSchema,
  pakeStartRequestSchema,
} from "@tomat/shared";
import { decodeBase64, encodeBase64 } from "@std/encoding/base64";
import { authService } from "../../services/auth.ts";
import { dropClientSettingsCache } from "@tomat/core-engine/services/core-settings";
import { isBehindProxy } from "../../services/deployment.ts";
import { tlsCertFingerprint } from "../../services/tls.ts";
import { parseBody, readJson } from "@tomat/core-engine/http/body";
import { bearerMiddleware, requireClient } from "../middleware/auth.ts";
import { AppError } from "@tomat/core-engine/http/middleware/errors";
import { getLogger } from "../../shared/log.ts";
import { wsHub } from "../../ws/hub.ts";

const log = getLogger("http.pairing");

export function pairingRoutes(): Hono {
  const r = new Hono();

  // Mint a pairing code. Two authorization paths:
  //   A) X-Admin-Token  - device access (the installer's mint-code step, "pair from this
  //      machine"). High-entropy; no rate limit needed.
  //   B) Bearer + password - an already-paired client minting remotely. The
  //      bearer proves the caller is a trusted device; the password is the
  //      second factor (argon2id + rate limited inside verifyAdminPassword).
  r.post("/codes", async (c) => {
    const body = parseBody(pairingCodeRequestSchema, await readJson(c, { allowEmpty: true }));
    const adminToken = c.req.header("x-admin-token");
    if (adminToken) {
      await authService().verifyAdminToken(adminToken);
    } else {
      const client = await authService().authenticate(bearerToken(c));
      authService().verifyAdminPassword(body.password ?? null, peerIp(c), client.id);
    }
    const result = authService().mintPairingCode(body.ttlSec);
    return c.json(result);
  });

  // PAKE step 1: the client runs the CPace initiator keyed by the pairing code
  // and sends its session id + first message. Core responds with its message.
  r.post("/pake/start", async (c) => {
    const body = parseBody(pakeStartRequestSchema, await readJson(c, { allowEmpty: true }));
    const result = authService().pakeStart(
      decodeBase64(body.sid),
      decodeBase64(body.msgA),
      body.clientName,
      peerIp(c),
      await pairingServerPin(),
    );
    return c.json({ pakeId: result.pakeId, msgB: encodeBase64(result.msgB) });
  });

  // PAKE step 2: the client confirms (its confirmation binds the cert pin it
  // observed). Core verifies it against its OWN pin; a wrong code or a MITM cert
  // both fail. On success core mints the token and returns its own confirmation.
  r.post("/pake/finish", async (c) => {
    const body = parseBody(pakeFinishRequestSchema, await readJson(c, { allowEmpty: true }));
    const result = await authService().pakeFinish(
      body.pakeId,
      decodeBase64(body.confirmC),
      await pairingServerPin(),
    );
    return c.json({
      token: result.token,
      clientId: result.clientId,
      coreVersion: result.coreVersion,
      confirmS: encodeBase64(result.confirmS),
    });
  });

  r.get("/clients", bearerMiddleware(), (c) => {
    const me = requireClient(c);
    return c.json(authService().listClients(me.id));
  });

  r.delete("/clients/:id", bearerMiddleware(), async (c) => {
    const me = requireClient(c);
    const id = c.req.param("id") === "me" ? me.id : c.req.param("id");
    // A client may always remove ITSELF (the "leave" action). Revoking a
    // DIFFERENT paired device is a privileged fleet action: without this gate
    // any single paired device could wipe and lock out every other device. It
    // is authorized the same two ways as minting a code: the admin token
    // (device access) OR the admin password (an already-paired client, body).
    if (id !== me.id) {
      const adminToken = c.req.header("x-admin-token");
      if (adminToken) {
        await authService().verifyAdminToken(adminToken);
      } else {
        const body = parseBody(clientRevokeRequestSchema, await readJson(c, { allowEmpty: true }));
        authService().verifyAdminPassword(body.password ?? null, peerIp(c), me.id);
      }
    }
    const { existed, attachmentPaths } = await authService().revokeClient(id);
    if (!existed) {
      throw new AppError("not_found", `no paired client with id ${id}`);
    }
    log.info(`client ${me.id} revoked client ${id}`);
    // Cut off any live WebSocket for the revoked client so revocation actually
    // removes access (the WS authenticates only once, at upgrade).
    wsHub().closeClient(id);
    // The client's per-client settings rows are cascade-deleted with its
    // `clients` row; drop the in-memory overlay cache to match.
    dropClientSettingsCache(id);
    // Best-effort cleanup of attachment files on disk. We've already
    // cascaded the DB rows so a stragglers-on-disk situation only wastes
    // bytes; log it but don't fail the request.
    for (const p of attachmentPaths) {
      try {
        await Deno.remove(p);
      } catch (err) {
        if (!(err instanceof Deno.errors.NotFound)) {
          log.warn(`revoke: failed to remove attachment ${p}: ${errMessage(err)}`);
        }
      }
    }
    return c.body(null, 204);
  });

  r.post("/rotate", bearerMiddleware(), async (c) => {
    const me = requireClient(c);
    const token = await authService().rotateToken(me.id);
    // Drop existing sockets: they were authenticated with the OLD token, which
    // is now invalid. The client reconnects /ws/v1 with the new token.
    wsHub().closeClient(me.id);
    return c.json({ token });
  });

  return r;
}

// The value folded into the pairing PAKE (channel id + key confirmation),
// binding the TLS channel to the handshake. Normally Core's own self-signed cert
// fingerprint, so a MITM that re-terminates TLS is detected. When Core is served
// behind a terminating HTTPS proxy (`server.behindProxy`), the Client validates
// the proxy's real cert via WebPKI and cannot observe Core's cert, so there is no
// shared value to bind: both sides fold empty. This is the security boundary for
// the trust mode - a self-signed Core ALWAYS folds its real fingerprint, so a
// client that folded empty (or a MITM cert) fails the confirmation. Empty on both
// sides is only reachable when the operator actually set `server.behindProxy`.
function pairingServerPin(): Promise<string> {
  return isBehindProxy() ? Promise.resolve("") : tlsCertFingerprint();
}

// The bearer token from the Authorization header, or null. authService()
// .authenticate turns null into a 401, so callers can pass it straight through.
function bearerToken(c: Context): string | null {
  const match = /^Bearer\s+(.+)$/i.exec(c.req.header("authorization") ?? "");
  return match ? match[1].trim() : null;
}

// The REAL socket peer address, threaded from Deno.serve into the Hono env (see
// main.ts). We deliberately do NOT trust X-Forwarded-For / X-Real-IP, which are
// client-settable: an attacker would rotate them to hand every guess a fresh,
// empty rate-limit bucket. Under `server.behindProxy` every request arrives from
// the proxy's address, so the per-IP limiter collapses to one shared bucket -
// stricter, and safe. Honoring XFF there would need a trusted-proxy setting.
function peerIp(c: Context): string {
  const peer = (c.env as { remoteAddr?: { hostname?: string } } | undefined)?.remoteAddr;
  return peer?.hostname ?? "local";
}
