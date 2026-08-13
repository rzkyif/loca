import { assertEquals } from "@std/assert";
import { scrubSecrets } from "./log.ts";

Deno.test("scrubSecrets: masks Bearer tokens", () => {
  const out = scrubSecrets("auth: Authorization: Bearer abc123XYZ_-token.value=,foo");
  // The bearer match is greedy on token chars; everything up to the comma
  // collapses to <REDACTED>.
  assertEquals(out.includes("abc123XYZ_-token.value"), false);
  assertEquals(out.includes("Bearer <REDACTED>"), true);
});

Deno.test("scrubSecrets: masks X-Admin-Token header values", () => {
  assertEquals(scrubSecrets("X-Admin-Token: deadbeef00112233"), "X-Admin-Token: <REDACTED>");
  assertEquals(scrubSecrets("x-admin-token=deadbeefDEADBEEF"), "x-admin-token=<REDACTED>");
});

Deno.test("scrubSecrets: masks token= URL params", () => {
  assertEquals(scrubSecrets("/ws/v1?token=abc-XYZ_123&other=1"), "/ws/v1?token=<REDACTED>&other=1");
});

Deno.test("scrubSecrets: masks bare base64url-shaped tokens (40+ chars)", () => {
  const tok = "A".repeat(43);
  assertEquals(
    scrubSecrets(`stray token in message: ${tok} after`),
    "stray token in message: <REDACTED> after",
  );
});

Deno.test("scrubSecrets: masks bare hex strings >= 32 chars", () => {
  const adminToken = "deadbeef".repeat(4); // 32 chars
  assertEquals(
    scrubSecrets(`admin token on disk: ${adminToken}`),
    "admin token on disk: <REDACTED>",
  );
  const sha = "abcd1234".repeat(8); // 64 chars
  assertEquals(scrubSecrets(`hash: ${sha}`), "hash: <REDACTED>");
});

Deno.test("scrubSecrets: leaves short identifiers alone", () => {
  // 6-digit pairing codes, short ids, file basenames, paths.
  const msg = "minted pairing code 123456; root: /Users/x/.loca/core; extension: builtin";
  assertEquals(scrubSecrets(msg), msg);
});

Deno.test("scrubSecrets: handles multiple secrets on one line", () => {
  const out = scrubSecrets(
    "Bearer abc123XYZ_4567890123456789 then x-admin-token: ffffffffffffffffffffffffffffffff done",
  );
  assertEquals(out.includes("abc123XYZ_4567890123456789"), false);
  assertEquals(out.includes("ffffffffffffffffffffffffffffffff"), false);
  assertEquals(out.includes("<REDACTED>"), true);
  assertEquals(out.includes("done"), true);
});
