// Tests for the CPace balanced PAKE (ciphersuite CPACE-RISTRETTO255-SHA512).
//
// The acceptance gate is test #1: it reproduces the official appendix vector
// from draft-irtf-cfrg-cpace-21 (April 2026), Appendix B.3 (CPace with group
// ristretto255 and hash SHA-512), initiator/responder mode (B.3.5). Every
// intermediate (generator g, Ya, Yb, K, raw 64-byte ISK) is checked against the
// draft's bytes.

import { assert, assertEquals, assertNotEquals, assertThrows } from "@std/assert";

import {
  __test,
  confirmTag,
  cpaceInitiatorStart,
  cpaceResponder,
  randomSid,
  verifyConfirm,
} from "./pake.ts";

// --- hex helpers ----------------------------------------------------------

function hex(s: string): Uint8Array {
  const clean = s.replace(/\s+/g, "");
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function toHex(u: Uint8Array): string {
  return [...u].map((b) => b.toString(16).padStart(2, "0")).join("");
}

const utf8 = (s: string) => new TextEncoder().encode(s);

// --- the B.3 vector (draft-irtf-cfrg-cpace-21, Appendix B.3) ---------------

const VEC = {
  // B.3.1 inputs
  PRS: utf8("Password"),
  // CI = b'\x0bA_initiator\x0bB_responder' = lv("A_initiator")||lv("B_responder")
  CI: hex("0b415f696e69746961746f720b425f726573706f6e646572"),
  sid: hex("7e4b4791d6a8ef019b936c79fb7f2c57"),
  // B.3.1 outputs
  genStr: hex(
    "11435061636552697374726574746f3235350850617373776f7264" +
      "64" +
      "00".repeat(100) +
      "180b415f696e69746961746f720b425f726573706f6e646572107e4b4791d6a8ef019b936c79fb7f2c57",
  ),
  genHash: hex(
    "da6d3ddc8802fca9058755ffd3ebde08a9c2c74945901a258482a288" +
      "b6663af06bf645c93cd1c51512307199c80e84908916d983b34af772" +
      "05f90851a657ee27",
  ),
  g: hex("222b6b195fe84b1652badb6f6a3ae3d24341e7306967f0b8115b40d5698c7e56"),
  // B.3.2 message from A
  ADa: utf8("ADa"),
  ya: hex("da3d23700a9e5699258aef94dc060dfda5ebb61f02a5ea77fad53f4ff0976d08"),
  Ya: hex("d6bac480f2c386c394efc7c47adb9925dcd2630b64f240c50f8d0eec482b9157"),
  // B.3.3 message from B
  ADb: utf8("ADb"),
  yb: hex("d2316b454718c35362d83d69df6320f38578ed5984651435e2949762d900b80d"),
  Yb: hex("3ea7e0b19560d7c0b0f5734f63b955286dfa8232b5ebe63324e2d9e7433f7258"),
  // B.3.4 secret point K
  K: hex("80b69a8a76457ab6a4d7f887a4bf6b55a2f80ac19c333f917a05fc9887c8b40f"),
  // B.3.5 ISK (initiator/responder, transcript_ir)
  transcriptIr: hex(
    "20d6bac480f2c386c394efc7c47adb9925dcd2630b64f240c50f8d0e" +
      "ec482b915703414461203ea7e0b19560d7c0b0f5734f63b955286dfa" +
      "8232b5ebe63324e2d9e7433f725803414462",
  ),
  iskInput: hex(
    "15435061636552697374726574746f3235355f49534b107e4b4791d6" +
      "a8ef019b936c79fb7f2c572080b69a8a76457ab6a4d7f887a4bf6b55" +
      "a2f80ac19c333f917a05fc9887c8b40f20d6bac480f2c386c394efc7" +
      "c47adb9925dcd2630b64f240c50f8d0eec482b915703414461203ea7" +
      "e0b19560d7c0b0f5734f63b955286dfa8232b5ebe63324e2d9e7433f" +
      "725803414462",
  ),
  ISK: hex(
    "b69effbf61b51d56401c0f65601abe428de8206feaaf0e32198896dc" +
      "ae7b35cd2b38950a39dfd5d4a79164614c2984f7daa460b588c1e80c" +
      "3fa2068af7900447",
  ),
};

// --- test #1: official vector reproduction (the acceptance gate) ----------

Deno.test("B.3 vector: generator_string matches", () => {
  const genStr = __test.generatorString(VEC.PRS, VEC.CI, VEC.sid);
  assertEquals(toHex(genStr), toHex(VEC.genStr));
});

Deno.test("B.3 vector: calculate_generator produces g", () => {
  const g = __test.calculateGenerator(VEC.PRS, VEC.CI, VEC.sid);
  assertEquals(toHex(g.toBytes()), toHex(VEC.g));
});

Deno.test("B.3 vector: Ya = scalar_mult(ya, g)", () => {
  const g = __test.calculateGenerator(VEC.PRS, VEC.CI, VEC.sid);
  const Ya = __test.pointToBytes(VEC.ya, g);
  assertEquals(toHex(Ya), toHex(VEC.Ya));
});

Deno.test("B.3 vector: Yb = scalar_mult(yb, g)", () => {
  const g = __test.calculateGenerator(VEC.PRS, VEC.CI, VEC.sid);
  const Yb = __test.pointToBytes(VEC.yb, g);
  assertEquals(toHex(Yb), toHex(VEC.Yb));
});

Deno.test("B.3 vector: K = scalar_mult_vfy(ya, Yb) = scalar_mult_vfy(yb, Ya)", () => {
  const ya = __test.clampScalarLe(VEC.ya);
  const yb = __test.clampScalarLe(VEC.yb);
  const Ka = __test.scalarMultVfy(ya, VEC.Yb);
  const Kb = __test.scalarMultVfy(yb, VEC.Ya);
  assertEquals(toHex(Ka), toHex(VEC.K));
  assertEquals(toHex(Kb), toHex(VEC.K));
});

Deno.test("B.3 vector: transcript_ir matches", () => {
  const t = __test.transcriptIr(VEC.Ya, VEC.ADa, VEC.Yb, VEC.ADb);
  assertEquals(toHex(t), toHex(VEC.transcriptIr));
});

Deno.test("B.3 vector: raw ISK (initiator/responder) matches", () => {
  const isk = __test.computeIsk(VEC.sid, VEC.K, VEC.Ya, VEC.ADa, VEC.Yb, VEC.ADb);
  assertEquals(toHex(isk), toHex(VEC.ISK));
});

Deno.test("B.3 vector: end-to-end through internal seam yields the vector ISK", () => {
  // Drive both roles with the fixed CI/AD and fixed scalars from the vector.
  const fixedYa = () => __test.clampScalarLe(VEC.ya);
  const fixedYb = () => __test.clampScalarLe(VEC.yb);

  const initiator = __test.initiatorStart("Password", VEC.sid, VEC.CI, VEC.ADa, VEC.ADb, fixedYa);
  assertEquals(toHex(initiator.msgA), toHex(VEC.Ya));

  const responder = __test.responder(
    "Password",
    VEC.sid,
    initiator.msgA,
    VEC.CI,
    VEC.ADa,
    VEC.ADb,
    fixedYb,
  );
  assertEquals(toHex(responder.msgB), toHex(VEC.Yb));

  const initiatorKey = initiator.finish(responder.msgB);
  // Public API truncates the raw 64-byte ISK to 32 bytes; both sides agree.
  assertEquals(toHex(initiatorKey), toHex(VEC.ISK.slice(0, 32)));
  assertEquals(toHex(responder.isk), toHex(VEC.ISK.slice(0, 32)));
});

// --- test #2: full round-trip on the public API ---------------------------

Deno.test("round-trip: same password agrees, different password differs", () => {
  const sid = randomSid();
  assertEquals(sid.length, 16);

  const a = cpaceInitiatorStart("123456", sid);
  const b = cpaceResponder("123456", sid, a.msgA);
  const aKey = a.finish(b.msgB);
  assertEquals(toHex(aKey), toHex(b.isk));
  assertEquals(aKey.length, 32);

  // Different passwords must not agree.
  const a2 = cpaceInitiatorStart("123456", sid);
  const b2 = cpaceResponder("654321", sid, a2.msgA);
  const a2Key = a2.finish(b2.msgB);
  assertNotEquals(toHex(a2Key), toHex(b2.isk));
});

Deno.test("channel identifier (cert pin) is bound into the key", () => {
  const sid = randomSid();
  const code = "123456";
  const pinA = new TextEncoder().encode("cert-pin-A");
  const pinB = new TextEncoder().encode("cert-pin-B");

  // Same code + same pin → agree.
  const a = cpaceInitiatorStart(code, sid, pinA);
  const b = cpaceResponder(code, sid, a.msgA, pinA);
  assertEquals(toHex(a.finish(b.msgB)), toHex(b.isk));

  // Same code but DIFFERENT pins (the MITM case: client saw a different cert
  // than core has) → keys diverge, so the handshake can't complete.
  const a2 = cpaceInitiatorStart(code, sid, pinA);
  const b2 = cpaceResponder(code, sid, a2.msgA, pinB);
  assertNotEquals(toHex(a2.finish(b2.msgB)), toHex(b2.isk));

  // Empty vs cert-bound (a webpki-mode client against a core that is NOT behind
  // a proxy, or the reverse) → diverges the same way: the empty fold agrees only
  // when BOTH sides deliberately fold nothing.
  const a3 = cpaceInitiatorStart(code, sid);
  const b3 = cpaceResponder(code, sid, a3.msgA, pinA);
  assertNotEquals(toHex(a3.finish(b3.msgB)), toHex(b3.isk));
});

// --- test #3: invalid peer messages are rejected --------------------------

Deno.test("responder rejects identity / wrong-length msgA", () => {
  const sid = randomSid();
  // Identity encoding (all zeros) decodes to the neutral element.
  assertThrows(() => cpaceResponder("123456", sid, new Uint8Array(32)));
  // Wrong length fails to decode.
  assertThrows(() => cpaceResponder("123456", sid, new Uint8Array(31)));
});

Deno.test("initiator.finish rejects identity / wrong-length msgB", () => {
  const sid = randomSid();
  const a = cpaceInitiatorStart("123456", sid);
  assertThrows(() => a.finish(new Uint8Array(32)));

  const a2 = cpaceInitiatorStart("123456", sid);
  assertThrows(() => a2.finish(new Uint8Array(31)));
});

Deno.test("finish can only be called once", () => {
  const sid = randomSid();
  const a = cpaceInitiatorStart("123456", sid);
  const b = cpaceResponder("123456", sid, a.msgA);
  a.finish(b.msgB);
  assertThrows(() => a.finish(b.msgB));
});

// --- test #4: confirmation tag round-trip + tamper detection --------------

Deno.test("confirmTag / verifyConfirm round-trip and tamper detection", () => {
  const isk = randomSid().length ? new Uint8Array(32).fill(7) : new Uint8Array(32);
  const msgA = new Uint8Array(32).fill(1);
  const msgB = new Uint8Array(32).fill(2);
  const pin = "cert-pin-abc";

  const tagC = confirmTag(isk, "C", msgA, msgB, pin);
  assertEquals(tagC.length, 32);
  assert(verifyConfirm(tagC, isk, "C", msgA, msgB, pin));

  // Role differs.
  assert(!verifyConfirm(tagC, isk, "S", msgA, msgB, pin));

  // A byte of msgA flipped.
  const msgABad = msgA.slice();
  msgABad[0] ^= 0x01;
  assert(!verifyConfirm(tagC, isk, "C", msgABad, msgB, pin));

  // A byte of msgB flipped.
  const msgBBad = msgB.slice();
  msgBBad[5] ^= 0x80;
  assert(!verifyConfirm(tagC, isk, "C", msgA, msgBBad, pin));

  // Pin differs.
  assert(!verifyConfirm(tagC, isk, "C", msgA, msgB, "cert-pin-xyz"));

  // A cert-bound tag never verifies against the empty fold (and vice versa):
  // the webpki empty fold agrees only when both sides deliberately use it.
  assert(!verifyConfirm(tagC, isk, "C", msgA, msgB, ""));
  const tagEmpty = confirmTag(isk, "C", msgA, msgB, "");
  assert(verifyConfirm(tagEmpty, isk, "C", msgA, msgB, ""));
  assert(!verifyConfirm(tagEmpty, isk, "C", msgA, msgB, pin));

  // Tag byte flipped.
  const tagBad = tagC.slice();
  tagBad[31] ^= 0x01;
  assert(!verifyConfirm(tagBad, isk, "C", msgA, msgB, pin));

  // "S" role produces a distinct, self-consistent tag.
  const tagS = confirmTag(isk, "S", msgA, msgB, pin);
  assertNotEquals(toHex(tagS), toHex(tagC));
  assert(verifyConfirm(tagS, isk, "S", msgA, msgB, pin));
});
