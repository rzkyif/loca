// pairing-flow Zod schemas. These are the wire contract for the
// most-exposed surface (paired-client onboarding), so locking them down
// against contract drift is worth a few targeted assertions.

import { assertEquals } from "@std/assert";
import {
  pairingCodeRequestSchema,
  pakeFinishRequestSchema,
  pakeStartRequestSchema,
} from "./pairing.ts";

const B64_32 = btoa(String.fromCharCode(...new Uint8Array(32)));

Deno.test("pairingCodeRequestSchema: accepts empty body (defaults)", () => {
  const parsed = pairingCodeRequestSchema.parse({});
  assertEquals(parsed, {});
});

Deno.test("pairingCodeRequestSchema: enforces ttlSec bounds (60 .. 3600)", () => {
  assertEquals(pairingCodeRequestSchema.safeParse({ ttlSec: 60 }).success, true);
  assertEquals(pairingCodeRequestSchema.safeParse({ ttlSec: 3600 }).success, true);
  assertEquals(pairingCodeRequestSchema.safeParse({ ttlSec: 59 }).success, false);
  assertEquals(pairingCodeRequestSchema.safeParse({ ttlSec: 3601 }).success, false);
  assertEquals(pairingCodeRequestSchema.safeParse({ ttlSec: 100.5 }).success, false);
});

Deno.test("pairingCodeRequestSchema: rejects unknown fields (strict mode)", () => {
  const result = pairingCodeRequestSchema.safeParse({ ttlSec: 60, extra: 1 });
  assertEquals(result.success, false);
});

Deno.test("pakeStartRequestSchema: accepts a well-formed start body", () => {
  const parsed = pakeStartRequestSchema.parse({
    clientName: "my-laptop",
    sid: B64_32,
    msgA: B64_32,
  });
  assertEquals(parsed.clientName, "my-laptop");
});

Deno.test("pakeStartRequestSchema: rejects empty / >64-char clientName", () => {
  assertEquals(
    pakeStartRequestSchema.safeParse({
      clientName: "",
      sid: B64_32,
      msgA: B64_32,
    }).success,
    false,
  );
  assertEquals(
    pakeStartRequestSchema.safeParse({
      clientName: "x".repeat(65),
      sid: B64_32,
      msgA: B64_32,
    }).success,
    false,
  );
});

Deno.test("pakeStartRequestSchema: rejects non-base64 msgA / sid", () => {
  assertEquals(
    pakeStartRequestSchema.safeParse({
      clientName: "x",
      sid: B64_32,
      msgA: "not base64!!",
    }).success,
    false,
  );
});

Deno.test("pakeStartRequestSchema: rejects unknown fields (strict mode)", () => {
  assertEquals(
    pakeStartRequestSchema.safeParse({
      clientName: "x",
      sid: B64_32,
      msgA: B64_32,
      sneaky: true,
    }).success,
    false,
  );
});

Deno.test("pakeFinishRequestSchema: accepts pakeId + base64 confirmC", () => {
  const parsed = pakeFinishRequestSchema.parse({
    pakeId: "abc123",
    confirmC: B64_32,
  });
  assertEquals(parsed.pakeId, "abc123");
});

Deno.test("pakeFinishRequestSchema: rejects missing/empty fields + unknowns", () => {
  assertEquals(pakeFinishRequestSchema.safeParse({ pakeId: "", confirmC: B64_32 }).success, false);
  assertEquals(pakeFinishRequestSchema.safeParse({ pakeId: "x", confirmC: "@@" }).success, false);
  assertEquals(
    pakeFinishRequestSchema.safeParse({
      pakeId: "x",
      confirmC: B64_32,
      extra: 1,
    }).success,
    false,
  );
});
