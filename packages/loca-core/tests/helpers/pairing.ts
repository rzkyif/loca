// In-process pairing helpers for tests.
//
// `pairClient` runs the full CPace PAKE handshake against the auth service and
// returns a real bearer token. It is the convenient replacement for the old
// `authService().claim(code, ...)` one-liner most route/service tests used.
// `pakeViaApp` drives the same handshake over `app.fetch()` for the route
// contract test (where the cert pin must come from the real tls service).

import { confirmTag, cpaceInitiatorStart, randomSid } from "@loca/shared";
import { decodeBase64, encodeBase64 } from "jsr:@std/encoding@^1.0.0/base64";
import { authService } from "../../src/services/auth.ts";

// Opaque cert pin used by the service-level helper. The PAKE only requires the
// two ends to agree on it (no MITM), so any fixed value works and we avoid
// generating real TLS material for tests that just need a token.
const TEST_PIN = "test-cert-pin";

/** Mint a code and complete the PAKE in-process; returns a real bearer token. */
export async function pairClient(
  name = "test-client",
  ip = "127.0.0.1",
): Promise<{ token: string; clientId: string }> {
  const auth = authService();
  const { code } = auth.mintPairingCode();
  const sid = randomSid();
  const ci = new TextEncoder().encode(TEST_PIN);
  const init = cpaceInitiatorStart(code, sid, ci);
  const { pakeId, msgB } = auth.pakeStart(sid, init.msgA, name, ip, TEST_PIN);
  const isk = init.finish(msgB);
  const confirmC = confirmTag(isk, "C", init.msgA, msgB, TEST_PIN);
  const { token, clientId } = await auth.pakeFinish(pakeId, confirmC, TEST_PIN);
  return { token, clientId };
}

/**
 * Drive the PAKE over the HTTP app for a given code. `observedPin` is the cert
 * pin the simulated client saw: pass the real `tlsCertFingerprint()` for the
 * happy path, or a different value to simulate a MITM-substituted cert. Returns
 * the raw `/pake/finish` Response so callers can assert status + body.
 */
export async function pakeViaApp(
  app: { fetch: (req: Request) => Response | Promise<Response> },
  code: string,
  clientName: string,
  observedPin: string,
): Promise<Response> {
  const sid = randomSid();
  // The client folds the pin IT observed into the CPace channel id; the route
  // folds core's real pin server-side, so a mismatch (MITM) diverges the keys.
  const init = cpaceInitiatorStart(code, sid, new TextEncoder().encode(observedPin));
  const startRes = await app.fetch(
    jsonReq("http://x/api/v1/pairing/pake/start", {
      clientName,
      sid: encodeBase64(sid),
      msgA: encodeBase64(init.msgA),
    }),
  );
  if (startRes.status !== 200) return startRes;
  const { pakeId, msgB } = await startRes.json();
  const msgBBytes = decodeBase64(msgB);
  const isk = init.finish(msgBBytes);
  const confirmC = confirmTag(isk, "C", init.msgA, msgBBytes, observedPin);
  return await app.fetch(
    jsonReq("http://x/api/v1/pairing/pake/finish", {
      pakeId,
      confirmC: encodeBase64(confirmC),
    }),
  );
}

function jsonReq(url: string, body: unknown): Request {
  return new Request(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}
