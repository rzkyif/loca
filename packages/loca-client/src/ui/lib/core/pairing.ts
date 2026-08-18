// Pairing client.
//
// Pairing is a CPace PAKE keyed by the 6-digit code. We first TOFU-capture the
// core's cert pin (the cert isn't pinned yet), then fold that pin into BOTH the
// CPace channel identifier (so the derived key itself is cert-bound) AND the key
// confirmation. A MITM that re-terminates TLS with a different cert is therefore
// detected twice over: the keys diverge, and `verifyConfirm` on core's
// `confirmS` fails, so we refuse to pair. On success we return the bearer token
// AND the pin to store, so every later connection enforces it.
//
// `PairingApi` (instance) covers operations on an ALREADY-paired core (mint a
// code for another client, list/revoke/rotate); those go through the pinned
// CoreClient. The free functions cover the UNPAIRED flow (probe, mint-with-
// admin-token, pair) where there is no pin yet.

import type {
  PairedClientEntry,
  PairingCodeRequest,
  PairingCodeResponse,
  PakeFinishResponse,
  PakeStartResponse,
} from "@loca/shared";
import { confirmTag, cpaceInitiatorStart, randomSid, verifyConfirm } from "@loca/shared";
import { type NetResponse, platform } from "../platform/index.ts";
import type { CoreClient, TrustMode } from "./client";

export interface PairResult {
  token: string;
  clientId: string;
  coreVersion: string;
  /** How to trust this Core's TLS on later connections. `pin` stores + enforces
   *  the captured cert pin; `webpki` validates the proxy's real cert (no pin). */
  trustMode: TrustMode;
  /** The cert pin captured at pairing; present (and enforced) only in pin mode. */
  tlsPin?: string;
}

export class PairingApi {
  constructor(private readonly client: CoreClient) {}

  // Mint a code on this (already-paired) core. Requires the on-disk admin token.
  // Goes through the client's own transport rather than reaching for
  // `platform().net`: this is an operation on an ALREADY-SELECTED core, so
  // whichever transport reaches that core is the right one.
  async mintCode(adminToken: string, req: PairingCodeRequest = {}): Promise<PairingCodeResponse> {
    const res = await this.client.fetchRaw({
      url: `${this.client.endpoint.baseUrl}/api/v1/pairing/codes`,
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Admin-Token": adminToken,
      },
      body: JSON.stringify(req),
    });
    if (!ok(res)) throw new Error(`mint pairing code: HTTP ${res.status}`);
    return JSON.parse(text(res)) as PairingCodeResponse;
  }

  // Mint a code on this (already-paired) core using the admin PASSWORD instead
  // of the on-disk token. The bearer (this.client) proves we're a paired
  // device; the password is the second factor. This is the remote path: it
  // needs no device access to the core's filesystem.
  mintCodeWithPassword(
    password: string,
    req: PairingCodeRequest = {},
  ): Promise<PairingCodeResponse> {
    return this.client.post("/api/v1/pairing/codes", { ...req, password });
  }

  listClients(): Promise<PairedClientEntry[]> {
    return this.client.get("/api/v1/pairing/clients");
  }

  // Revoke a client. Removing OURSELVES (the "leave" action) needs no password.
  // Removing ANOTHER device is privileged: pass the admin password, sent in the
  // request body over the pinned TLS channel.
  revoke(clientId: string, password?: string): Promise<void> {
    return this.client.del(
      `/api/v1/pairing/clients/${encodeURIComponent(clientId)}`,
      password ? { password } : undefined,
    ) as Promise<void>;
  }

  rotate(): Promise<{ token: string }> {
    return this.client.post("/api/v1/pairing/rotate", {});
  }
}

// --- unpaired flow (no pin yet) -------------------------------------------

/** Probe a core's health over a TOFU TLS connection. Returns the reported
 *  version, its `behindProxy` hint (whether it is served over HTTPS by a
 *  terminating reverse proxy), and whether an admin password is set. The
 *  behindProxy hint selects the pairing trust mode; it is unauthenticated, but
 *  the PAKE cert fold makes a forged value fail closed (see `pairWithCode`),
 *  so it is a convenience, not a trust decision. */
export async function probeCore(
  baseUrl: string,
): Promise<{ version: string; behindProxy: boolean; adminPasswordSet: boolean }> {
  const res = await platform().net.fetch({
    url: `${baseUrl}/api/v1/health`,
    mode: "capture",
  });
  if (!ok(res)) throw new Error(`Core responded ${res.status}`);
  const body = JSON.parse(text(res)) as {
    version?: unknown;
    behindProxy?: unknown;
    adminPasswordSet?: unknown;
  };
  return {
    version: typeof body.version === "string" ? body.version : "unknown",
    behindProxy: body.behindProxy === true,
    adminPasswordSet: body.adminPasswordSet === true,
  };
}

/** Set or replace the admin password on a local core, authorized by its
 *  on-disk admin token, over TOFU TLS. The installer never asks for one (it
 *  runs non-interactively), so Settings > Cores offers this on the local Core.
 *  The password lets this and other paired devices mint codes / revoke
 *  remotely afterward. */
export async function setAdminPasswordWithToken(
  baseUrl: string,
  adminToken: string,
  password: string,
): Promise<void> {
  const res = await platform().net.fetch({
    url: `${baseUrl}/api/v1/admin/password`,
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-Admin-Token": adminToken,
    },
    body: JSON.stringify({ password }),
    mode: "capture",
  });
  if (!ok(res)) throw apiError(res, "set admin password");
}

/** Mint a code on an unpaired core via its admin token, over TOFU TLS. */
export async function mintCodeWithAdminToken(
  baseUrl: string,
  adminToken: string,
  req: PairingCodeRequest = {},
): Promise<PairingCodeResponse> {
  const res = await platform().net.fetch({
    url: `${baseUrl}/api/v1/pairing/codes`,
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-Admin-Token": adminToken,
    },
    body: JSON.stringify(req),
    mode: "capture",
  });
  if (!ok(res)) throw apiError(res, "mint pairing code");
  return JSON.parse(text(res)) as PairingCodeResponse;
}

/** Run the full PAKE pairing against a core, proving the 6-digit code and
 *  binding the TLS channel. `behindProxy` (from the health probe) picks the
 *  trust mode: false (default) TOFU-captures the self-signed cert and folds its
 *  pin into the CPace channel id + confirmation; true validates the proxy's real
 *  cert via WebPKI and folds nothing (the Core folds empty too). Either way the
 *  fold is the security boundary: if the two sides folded different values,
 *  core's confirmation fails and we refuse to pair, so a forged `behindProxy`
 *  hint or a MITM cert both fail closed. */
export async function pairWithCode(
  baseUrl: string,
  clientName: string,
  code: string,
  behindProxy: boolean,
): Promise<PairResult> {
  const trustMode: TrustMode = behindProxy ? "webpki" : "pin";

  // Pin mode: TOFU-capture the server's cert pin (health is unauthenticated) and
  // fold it. WebPKI mode: the Client validates the proxy's real cert instead, so
  // there is no Core cert to observe; both sides fold empty.
  let pin: string | undefined;
  if (!behindProxy) {
    const probe = await platform().net.fetch({
      url: `${baseUrl}/api/v1/health`,
      mode: "capture",
    });
    pin = probe.capturedPin;
    if (!pin) throw new Error("could not read the Core's TLS certificate");
  }
  // The value folded into the CPace channel id + confirmation MAC. Empty in
  // webpki mode, matching the empty fold the Core uses when server.behindProxy.
  const foldPin = pin ?? "";
  const trust = pin ? { mode: trustMode, pin } : { mode: trustMode };

  const sid = randomSid();
  const ci = new TextEncoder().encode(foldPin);
  const init = cpaceInitiatorStart(code, sid, ci);

  // Step 1: start, enforcing the trust mode we chose (pin or webpki).
  const startRes = await platform().net.fetch({
    url: `${baseUrl}/api/v1/pairing/pake/start`,
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      clientName,
      sid: toBase64(sid),
      msgA: toBase64(init.msgA),
    }),
    ...trust,
  });
  if (!ok(startRes)) throw apiError(startRes, "pairing start failed");
  const start = JSON.parse(text(startRes)) as PakeStartResponse;
  const msgB = fromBase64(start.msgB);
  const isk = init.finish(msgB);

  // Step 2: finish. Our confirmation binds the same fold value.
  const confirmC = confirmTag(isk, "C", init.msgA, msgB, foldPin);
  const finishRes = await platform().net.fetch({
    url: `${baseUrl}/api/v1/pairing/pake/finish`,
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      pakeId: start.pakeId,
      confirmC: toBase64(confirmC),
    }),
    ...trust,
  });
  if (!ok(finishRes)) throw apiError(finishRes, "pairing failed");
  const fin = JSON.parse(text(finishRes)) as PakeFinishResponse;

  // Authenticate core: its confirmation must verify against the fold we used. A
  // MITM cert (pin mode) or a Core whose behindProxy setting disagrees with our
  // detected mode both make this fail.
  const serverOk = verifyConfirm(fromBase64(fin.confirmS), isk, "S", init.msgA, msgB, foldPin);
  if (!serverOk) {
    throw new Error(
      behindProxy
        ? "Core authentication failed: this Core is not configured to be served " +
            "over HTTPS (or a certificate could not be verified). Not paired."
        : "Core authentication failed: the TLS certificate could not be verified " +
            "(possible man-in-the-middle). Not paired.",
    );
  }

  return {
    token: fin.token,
    clientId: fin.clientId,
    coreVersion: fin.coreVersion,
    trustMode,
    tlsPin: pin,
  };
}

// --- helpers --------------------------------------------------------------

function ok(res: NetResponse): boolean {
  return res.status >= 200 && res.status < 300;
}

function text(res: NetResponse): string {
  return new TextDecoder().decode(res.body);
}

function apiError(res: NetResponse, fallback: string): Error {
  try {
    const body = JSON.parse(text(res)) as { error?: { message?: string } };
    if (body.error?.message) return new Error(body.error.message);
  } catch {
    /* */
  }
  return new Error(`${fallback}: HTTP ${res.status}`);
}

function toBase64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
  return btoa(s);
}

function fromBase64(str: string): Uint8Array {
  const bin = atob(str);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
