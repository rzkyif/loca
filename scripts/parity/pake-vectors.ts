// Generates the cross-implementation CPace fixtures the Rust port replays.
//
// The CFRG Appendix B.3 vector exercises exactly one input and never reaches
// multi-byte LEB128, the generator-string zero-pad clamp, or the confirmation
// tag. This script drives the REAL `@loca/shared` implementation through the
// cases that do, recording every intermediate, so the Rust side is checked
// against the code the Client actually runs.
//
// Deterministic by construction: the case inputs come from a seeded mulberry32,
// never from the CSPRNG, so a regeneration reproduces the file byte for byte.
//
// Usage:
//   deno run --allow-read --allow-write \
//     scripts/parity/pake-vectors.ts \
//     packages/loca-core-rs/src/auth/testdata/pake-vectors.json

import { __test, confirmTag } from "../../packages/loca-shared/src/crypto/pake.ts";

const utf8 = (s: string): Uint8Array => new TextEncoder().encode(s);
const toHex = (u: Uint8Array): string =>
  [...u].map((b) => b.toString(16).padStart(2, "0")).join("");

/** mulberry32: a small, well-distributed, fully deterministic PRNG. */
function prng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const rand = prng(0x10ca7e57);
const randomBytes = (n: number): Uint8Array =>
  Uint8Array.from({ length: n }, () => Math.floor(rand() * 256));
/** Printable ASCII, so a case's inputs stay readable in the fixture file. */
const randomAscii = (n: number): string =>
  Array.from({ length: n }, () => String.fromCharCode(33 + Math.floor(rand() * 94))).join("");

interface Case {
  note: string;
  password: string;
  sid: string;
  ci: string;
  ada: string;
  adb: string;
  ya: string;
  yb: string;
  pin: string;
}

/** A scalar whose top nibble is set, so the 252-bit clamp has work to do. */
function highNibbleScalar(): Uint8Array {
  const b = randomBytes(32);
  b[31] |= 0xf0;
  return b;
}

function newCase(note: string, over: Partial<Case> = {}): Case {
  return {
    note,
    password: "123456",
    sid: toHex(randomBytes(16)),
    ci: toHex(randomBytes(32)),
    ada: "",
    adb: "",
    ya: toHex(randomBytes(32)),
    yb: toHex(randomBytes(32)),
    pin: randomAscii(44),
    ...over,
  };
}

const cases: Case[] = [];

// The production shape: a 6-digit code, and the base64 SPKI pin folded into both
// the channel identifier and the confirmation.
{
  const pin = "IhIzenzVpWG2FDIvNyEqjqYAqxnMWJRPRJPRJPRJPRI=";
  cases.push(
    newCase("production: 6-digit code, base64 SPKI pin as CI", {
      ci: toHex(utf8(pin)),
      pin,
    }),
  );
}

// Behind a terminating HTTPS proxy both sides deliberately fold nothing. An
// implementation that treated "empty" as "skip the field" rather than "a
// zero-length length-prefixed field" would agree with itself and with nobody.
cases.push(newCase("behind-proxy: empty CI and empty pin", { ci: "", pin: "" }));

// prepend_len is LEB128, so 127 is one byte and 128 is two. Production never
// crosses it (a 44-char pin, a 6-digit code), which is exactly why it is the
// boundary a hand implementation gets wrong and never notices.
for (const n of [127, 128, 129, 200]) {
  cases.push(
    newCase(`CI of ${n} bytes: LEB128 single-byte to multi-byte boundary`, {
      ci: toHex(randomBytes(n)),
    }),
  );
}

// len_zpad = MAX(0, 128 - len(prepend_len(PRS)) - len(prepend_len(DSI)) - 1),
// so a long password walks it down to 0 and then holds it there.
for (const n of [100, 105, 108, 110, 300]) {
  cases.push(
    newCase(`password of ${n} bytes: the len_zpad clamp reaching 0`, {
      password: randomAscii(n),
    }),
  );
}

// lv_cat over a zero-length field, and over one long enough to need two length
// bytes of its own.
for (const n of [0, 1, 127, 128, 255]) {
  cases.push(newCase(`sid of ${n} bytes`, { sid: toHex(randomBytes(n)) }));
}

// lv2 in the confirmation tag is a 2-byte little-endian prefix, so a pin past
// 255 bytes is where a 1-byte prefix would silently truncate.
for (const n of [255, 256, 257, 1000]) {
  cases.push(newCase(`pin of ${n} bytes: lv2's 2-byte length prefix`, { pin: randomAscii(n) }));
}

// PRS and the confirmation pin are UTF-8 encoded, not latin1 and not UTF-16, so
// a multi-byte code point must contribute its encoded length to prepend_len and
// to lv2, not one byte per code unit.
cases.push(
  newCase("password and pin with two-byte code points", {
    password: "sandi-räháßia",
    pin: "sidik-jari-señor",
  }),
);
cases.push(
  newCase("password and pin with three- and four-byte code points", {
    password: "合言葉\u{1f512}",
    pin: "секрет\u{1f5dd}",
  }),
);

// Associated data is empty everywhere in Loca, but transcript_ir carries it and
// the appendix vector uses it, so the fixture exercises both.
cases.push(
  newCase("non-empty associated data on both sides", {
    ada: toHex(utf8("ADa")),
    adb: toHex(utf8("ADb")),
  }),
);
cases.push(
  newCase("associated data past the LEB128 boundary", {
    ada: toHex(randomBytes(130)),
    adb: toHex(randomBytes(200)),
  }),
);

// sample_scalar clears the 4 bits above group_size_bits before decoding.
cases.push(
  newCase("scalars whose top nibble is set: the 252-bit clamp", {
    ya: toHex(highNibbleScalar()),
    yb: toHex(highNibbleScalar()),
  }),
);

for (let i = 0; i < 64; i++) cases.push(newCase(`random ${i}`));

// --- run every case through the real implementation ------------------------

const hex = (s: string): Uint8Array => {
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.slice(i * 2, i * 2 + 2), 16);
  return out;
};

const vectors = cases.map((c) => {
  const prs = utf8(c.password);
  const sid = hex(c.sid);
  const ci = hex(c.ci);
  const ada = hex(c.ada);
  const adb = hex(c.adb);
  const yaScalar = () => __test.clampScalarLe(hex(c.ya));
  const ybScalar = () => __test.clampScalarLe(hex(c.yb));

  const genStr = __test.generatorString(prs, ci, sid);
  const g = __test.calculateGenerator(prs, ci, sid);

  const initiator = __test.initiatorStart(c.password, sid, ci, ada, adb, yaScalar);
  const responder = __test.responder(c.password, sid, initiator.msgA, ci, ada, adb, ybScalar);
  const initiatorKey = initiator.finish(responder.msgB);
  if (toHex(initiatorKey) !== toHex(responder.isk)) {
    throw new Error(`case "${c.note}": the two roles disagreed, which cannot happen`);
  }

  const k = __test.scalarMultVfy(ybScalar(), initiator.msgA);
  const rawIsk = __test.computeIsk(sid, k, initiator.msgA, ada, responder.msgB, adb);

  return {
    ...c,
    generatorString: toHex(genStr),
    g: toHex(g.toBytes()),
    msgA: toHex(initiator.msgA),
    msgB: toHex(responder.msgB),
    k: toHex(k),
    transcriptIr: toHex(__test.transcriptIr(initiator.msgA, ada, responder.msgB, adb)),
    rawIsk: toHex(rawIsk),
    isk: toHex(responder.isk),
    confirmC: toHex(confirmTag(responder.isk, "C", initiator.msgA, responder.msgB, c.pin)),
    confirmS: toHex(confirmTag(responder.isk, "S", initiator.msgA, responder.msgB, c.pin)),
  };
});

const out = Deno.args[0];
if (!out) throw new Error("usage: pake-vectors.ts <output.json>");
await Deno.writeTextFile(
  out,
  JSON.stringify(
    {
      source: "packages/loca-shared/src/crypto/pake.ts",
      generator: "scripts/parity/pake-vectors.ts",
      note: "Every field is produced by the TypeScript CPace the Client runs. Regenerate rather than hand-edit.",
      vectors,
    },
    null,
    2,
  ) + "\n",
);
console.log(`wrote ${vectors.length} vectors to ${out}`);
