// Admin-password hashing: hash → verify round-trip, wrong-password rejection,
// salt uniqueness, and parse robustness against a malformed stored string.

import { assertEquals } from "@std/assert";
import { hashAdminPassword, verifyAdminPassword } from "./password.ts";

Deno.test("hashAdminPassword: verifies the same password and rejects a wrong one", () => {
  const stored = hashAdminPassword("correct horse battery");
  assertEquals(verifyAdminPassword("correct horse battery", stored), true);
  assertEquals(verifyAdminPassword("wrong horse battery", stored), false);
  assertEquals(verifyAdminPassword("", stored), false);
});

Deno.test("hashAdminPassword: format is argon2id with a per-call random salt", () => {
  const a = hashAdminPassword("same-password");
  const b = hashAdminPassword("same-password");
  assertEquals(a.startsWith("argon2id$"), true);
  // Different salts → different stored strings even for an identical password.
  assertEquals(a !== b, true);
  // Both still verify.
  assertEquals(verifyAdminPassword("same-password", a), true);
  assertEquals(verifyAdminPassword("same-password", b), true);
});

Deno.test("verifyAdminPassword: returns false (never throws) on a malformed stored string", () => {
  for (const bad of ["", "nonsense", "argon2id$$$", "argon2id$m=1$onlythree", "bcrypt$x$y$z"]) {
    assertEquals(verifyAdminPassword("pw", bad), false);
  }
});
