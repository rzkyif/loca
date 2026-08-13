// AuthService pairing: code mint, the CPace PAKE handshake (happy path, wrong
// code, MITM cert-pin mismatch, attempt cap, TTL expiry, per-IP rate limit),
// and bearer auth. Uses a tempdir-isolated SQLite DB and a Date.now() mock for
// time-sensitive assertions.

import { assertEquals, assertRejects } from "@std/assert";
import { confirmTag, cpaceInitiatorStart, randomSid } from "@loca/shared";
import { type AuthService, authService } from "./auth.ts";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { mockClock } from "../../tests/helpers/time.ts";
import { AppError } from "@loca/core-engine";
import { paths } from "../paths.ts";

// Core's "real" cert pin for these tests. A client that observed the same pin
// (no MITM) passes a matching value as `clientPin`.
const SERVER_PIN = "server-cert-pin";

interface PakeOpts {
  code: string;
  clientName?: string;
  ip?: string;
  serverPin?: string;
  /** The pin the simulated client observed; differs from serverPin under MITM. */
  clientPin?: string;
}

// Run the full client-side CPace handshake against the auth service.
async function runPake(
  auth: AuthService,
  opts: PakeOpts,
): Promise<{ token: string; clientId: string }> {
  const {
    code,
    clientName = "c",
    ip = "127.0.0.1",
    serverPin = SERVER_PIN,
    clientPin = serverPin,
  } = opts;
  const sid = randomSid();
  // Client folds the pin IT observed (clientPin) into the channel id; server
  // folds serverPin. Under MITM they differ, diverging the keys.
  const init = cpaceInitiatorStart(code, sid, new TextEncoder().encode(clientPin));
  const { pakeId, msgB } = auth.pakeStart(sid, init.msgA, clientName, ip, serverPin);
  const isk = init.finish(msgB);
  const confirmC = confirmTag(isk, "C", init.msgA, msgB, clientPin);
  const { token, clientId } = await auth.pakeFinish(pakeId, confirmC, serverPin);
  return { token, clientId };
}

Deno.test("mintPairingCode: returns a 6-digit code with a future expiry", async () => {
  const env = await setupTestEnv();
  try {
    const { code, expiresAtMs } = authService().mintPairingCode();
    assertEquals(/^\d{6}$/.test(code), true);
    assertEquals(typeof expiresAtMs, "number");
    assertEquals(expiresAtMs > Date.now(), true);
  } finally {
    await env.teardown();
  }
});

Deno.test("mintPairingCode: a new code supersedes prior unclaimed codes", async () => {
  const env = await setupTestEnv();
  try {
    const auth = authService();
    const first = auth.mintPairingCode();
    auth.mintPairingCode();
    // Pairing with the OLD code now fails confirmation: core responds with the
    // NEW code, so the handshakes derive different keys.
    await assertRejects(() => runPake(auth, { code: first.code }), AppError, "confirmation failed");
  } finally {
    await env.teardown();
  }
});

Deno.test("pake: happy path returns token + clientId, claims the code", async () => {
  const env = await setupTestEnv();
  try {
    const auth = authService();
    const { code } = auth.mintPairingCode();
    const { token, clientId } = await runPake(auth, {
      code,
      clientName: "my-laptop",
    });
    assertEquals(typeof token, "string");
    const authed = await auth.authenticate(token);
    assertEquals(authed.id, clientId);
    assertEquals(authed.name, "my-laptop");
    // The code is single-use: a second handshake finds no active code.
    await assertRejects(() => runPake(auth, { code }), AppError, "no active pairing code");
  } finally {
    await env.teardown();
  }
});

Deno.test("pake: a MITM-substituted cert (pin mismatch) fails confirmation", async () => {
  const env = await setupTestEnv();
  try {
    const auth = authService();
    const { code } = auth.mintPairingCode();
    // Client knows the right code but observed a DIFFERENT cert pin than core's:
    // exactly the active-MITM-at-pairing case. Confirmation must reject.
    await assertRejects(
      () =>
        runPake(auth, {
          code,
          serverPin: SERVER_PIN,
          clientPin: "attacker-cert-pin",
        }),
      AppError,
      "confirmation failed",
    );
  } finally {
    await env.teardown();
  }
});

Deno.test("pake: poisons the code after 5 failed confirmations", async () => {
  const env = await setupTestEnv();
  try {
    const auth = authService();
    const { code } = auth.mintPairingCode();
    const wrong = code.slice(0, 5) + ((Number(code[5]) + 1) % 10).toString();
    for (let i = 0; i < 5; i++) {
      await assertRejects(
        () => runPake(auth, { code: wrong, ip: `10.0.0.${i + 1}` }),
        AppError,
        "confirmation failed",
      );
    }
    // 6th attempt with the CORRECT code: the code is now poisoned (claimed), so
    // there is no active code to start a handshake against.
    await assertRejects(() => runPake(auth, { code }), AppError, "no active pairing code");
  } finally {
    await env.teardown();
  }
});

Deno.test("pake: rejects an expired pairing code", async () => {
  const env = await setupTestEnv();
  const clock = mockClock(1_700_000_000_000);
  try {
    const auth = authService();
    const { code } = auth.mintPairingCode(60); // 60-sec TTL
    clock.advance(61_000);
    await assertRejects(() => runPake(auth, { code }), AppError, "has expired");
  } finally {
    clock.restore();
    await env.teardown();
  }
});

Deno.test("ws ticket: mint then consume returns the client id exactly once", async () => {
  const env = await setupTestEnv();
  try {
    const auth = authService();
    const ticket = auth.mintWsTicket("client-42");
    assertEquals(auth.consumeWsTicket(ticket), "client-42");
    // Single-use: a replay of the same ticket is rejected.
    assertEquals(auth.consumeWsTicket(ticket), null);
  } finally {
    await env.teardown();
  }
});

Deno.test("ws ticket: an unknown ticket is rejected", async () => {
  const env = await setupTestEnv();
  try {
    assertEquals(authService().consumeWsTicket("never-minted"), null);
  } finally {
    await env.teardown();
  }
});

Deno.test("ws ticket: an expired ticket is rejected (and consumed)", async () => {
  const env = await setupTestEnv();
  const clock = mockClock(1_700_000_000_000);
  try {
    const auth = authService();
    const ticket = auth.mintWsTicket("client-7");
    clock.advance(31_000); // past the 30-sec TTL
    assertEquals(auth.consumeWsTicket(ticket), null);
  } finally {
    clock.restore();
    await env.teardown();
  }
});

Deno.test("pake: rate-limits the 21st start from the same IP", async () => {
  const env = await setupTestEnv();
  try {
    const auth = authService();
    // No active code, so each start throws "no active pairing code". But the
    // IP gate is checked first, so the bucket still fills.
    const msgA = new Uint8Array(32);
    for (let i = 0; i < 20; i++) {
      try {
        auth.pakeStart(randomSid(), msgA, "x", "192.168.1.1", SERVER_PIN);
      } catch {
        /* expected */
      }
    }
    assertRejects_(
      () => auth.pakeStart(randomSid(), msgA, "x", "192.168.1.1", SERVER_PIN),
      "too many pairing attempts",
    );
    // A different IP is unaffected (fails for a different reason).
    assertRejects_(
      () => auth.pakeStart(randomSid(), msgA, "x", "10.0.0.1", SERVER_PIN),
      "no active pairing code",
    );
  } finally {
    await env.teardown();
  }
});

Deno.test("authenticate: rejects missing, unknown, and revoked tokens", async () => {
  const env = await setupTestEnv();
  try {
    const auth = authService();
    await assertRejects(() => auth.authenticate(null), AppError, "missing");
    await assertRejects(() => auth.authenticate("not-a-real-token"), AppError, "not recognized");
    const { code } = auth.mintPairingCode();
    const { token, clientId } = await runPake(auth, { code, clientName: "c" });
    await auth.revokeClient(clientId);
    await assertRejects(() => auth.authenticate(token), AppError, "not recognized");
  } finally {
    await env.teardown();
  }
});

Deno.test("verifyAdminToken: matches the on-disk token via constant-time compare", async () => {
  const env = await setupTestEnv();
  try {
    const auth = authService();
    await Deno.writeTextFile(paths().adminTokenFile, "secret-admin\n");
    if (Deno.build.os !== "windows") {
      await Deno.chmod(paths().adminTokenFile, 0o600);
    }
    await auth.verifyAdminToken("secret-admin");
    await assertRejects(() => auth.verifyAdminToken("wrong"), AppError, "mismatch");
    await assertRejects(() => auth.verifyAdminToken(null), AppError, "missing");
  } finally {
    await env.teardown();
  }
});

Deno.test("admin password: set, hasAdminPassword, verify accepts correct + rejects wrong", async () => {
  const env = await setupTestEnv();
  try {
    const auth = authService();
    assertEquals(auth.hasAdminPassword(), false);
    await auth.setAdminPassword("a memorable pass");
    assertEquals(auth.hasAdminPassword(), true);
    // Correct password passes (no throw); wrong one throws admin_password_invalid.
    auth.verifyAdminPassword("a memorable pass", "127.0.0.1", "client-1");
    assertRejects_(() => auth.verifyAdminPassword("nope", "127.0.0.1", "client-1"), "incorrect");
  } finally {
    await env.teardown();
  }
});

Deno.test("admin password: verify with none set surfaces admin_password_required", async () => {
  const env = await setupTestEnv();
  try {
    const auth = authService();
    assertRejects_(
      () => auth.verifyAdminPassword("x", "127.0.0.1", "client-1"),
      "no admin password",
    );
    assertRejects_(() => auth.verifyAdminPassword(null, "127.0.0.1", "client-1"), "missing");
  } finally {
    await env.teardown();
  }
});

Deno.test("admin password: locks out after the per-key failure cap", async () => {
  const env = await setupTestEnv();
  try {
    const auth = authService();
    await auth.setAdminPassword("the real password");
    // 5 wrong guesses (PASSWORD_FAIL_MAX_PER_KEY) are "incorrect"; the 6th is
    // refused as rate-limited BEFORE argon2id runs - even the correct password
    // is rejected while locked.
    for (let i = 0; i < 5; i++) {
      assertRejects_(() => auth.verifyAdminPassword("wrong", "10.0.0.1", "c1"), "incorrect");
    }
    assertRejects_(
      () => auth.verifyAdminPassword("the real password", "10.0.0.1", "c1"),
      "too many",
    );
    // A different client/IP still has its own budget.
    auth.verifyAdminPassword("the real password", "10.0.0.2", "c2");
  } finally {
    await env.teardown();
  }
});

// pakeStart is synchronous, so assertRejects (which awaits) doesn't fit; this
// asserts a synchronous throw carrying the expected message.
function assertRejects_(fn: () => unknown, includes: string): void {
  let threw: unknown;
  try {
    fn();
  } catch (e) {
    threw = e;
  }
  if (!(threw instanceof AppError)) {
    throw new Error(`expected AppError, got ${threw}`);
  }
  if (!threw.message.includes(includes)) {
    throw new Error(`expected message to include "${includes}", got "${threw.message}"`);
  }
}
