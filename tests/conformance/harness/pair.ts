// The real pairing handshake, run as a real client would run it.
//
// Nothing here is a shortcut. The harness mints a code with the admin token it
// seeded (the installer's privilege, not an auth bypass), then completes the
// full CPace exchange using the SAME implementation the Client uses, importing
// it from @loca/shared rather than reimplementing it. That matters more here
// than convenience: CPace is one of the byte-exact parity surfaces, and a
// second implementation living inside the suite meant to DETECT drift in it
// would be able to drift along with the thing it is checking.
//
// The pin folded into the channel identifier and the confirmation is derived
// independently from the core's certificate (harness/tls.ts). It is
// self-checking: fold the wrong value and the handshake fails, which is what
// `pairWithWrongPin` asserts on purpose.

import { confirmTag, cpaceInitiatorStart, randomSid, verifyConfirm } from "@loca/shared";
import { decodeBase64, encodeBase64 } from "@std/encoding/base64";
import type { CoreUnderTest } from "./core.ts";

export interface PairedIdentity {
  clientId: string;
  token: string;
  coreVersion: string;
}

async function json(res: Response): Promise<Record<string, unknown>> {
  const text = await res.text();
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}: ${text}`);
  return JSON.parse(text) as Record<string, unknown>;
}

/** Mint a pairing code with the seeded admin token. */
export async function mintCode(core: CoreUnderTest): Promise<string> {
  const res = await fetch(`${core.baseUrl}/api/v1/pairing/codes`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-admin-token": core.adminToken },
    body: JSON.stringify({}),
  });
  return (await json(res)).code as string;
}

/** Run the full PAKE with an explicit fold value. `foldPin` is normally the
 *  core's real pin; the negative case passes a wrong one deliberately. */
export async function pairWithPin(
  core: CoreUnderTest,
  clientName: string,
  code: string,
  foldPin: string,
): Promise<PairedIdentity> {
  const sid = randomSid();
  const ci = new TextEncoder().encode(foldPin);
  const init = cpaceInitiatorStart(code, sid, ci);

  const start = await json(
    await fetch(`${core.baseUrl}/api/v1/pairing/pake/start`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        clientName,
        sid: encodeBase64(sid),
        msgA: encodeBase64(init.msgA),
      }),
    }),
  );

  const msgB = decodeBase64(start.msgB as string);
  const isk = init.finish(msgB);
  const confirmC = confirmTag(isk, "C", init.msgA, msgB, foldPin);

  const fin = await json(
    await fetch(`${core.baseUrl}/api/v1/pairing/pake/finish`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        pakeId: start.pakeId as string,
        confirmC: encodeBase64(confirmC),
      }),
    }),
  );

  // Authenticate the core in return. A core that folded a different value fails
  // here, which is the half of the handshake a client-only check would miss.
  const ok = verifyConfirm(
    decodeBase64(fin.confirmS as string),
    isk,
    "S",
    init.msgA,
    msgB,
    foldPin,
  );
  if (!ok) throw new Error("core confirmation did not verify: the folded pin disagreed");

  return {
    clientId: fin.clientId as string,
    token: fin.token as string,
    coreVersion: fin.coreVersion as string,
  };
}

/** Mint a code and pair with the core's real pin. */
export async function pair(core: CoreUnderTest, clientName: string): Promise<PairedIdentity> {
  return await pairWithPin(core, clientName, await mintCode(core), core.tlsPin);
}

/** Mint a single-use WS ticket, the way the Client does, so the bearer never
 *  travels in a URL. */
export async function wsTicket(core: CoreUnderTest, token: string): Promise<string> {
  const res = await fetch(`${core.baseUrl}/api/v1/ws/ticket`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}` },
  });
  return (await json(res)).ticket as string;
}
