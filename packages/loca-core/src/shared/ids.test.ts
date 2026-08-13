// ID generators are thin wrappers around ulid/randomUUID. The wrappers
// exist so call sites express intent (session vs message vs etc.). We test
// that intent never accidentally collapses to a shared counter, that the
// formats stay parseable, and that uniqueness holds across a meaningful
// batch.

import { assertEquals, assertMatch, assertNotEquals } from "@std/assert";
import {
  newAttachmentId,
  newCallId,
  newClientId,
  newJobId,
  newMessageId,
  newRequestId,
  newSessionId,
  newStreamId,
} from "@loca/core-engine";

// Crockford base32 ULID: 26 chars, [0-9A-HJKMNP-TV-Z].
const ULID_PATTERN = /^[0-9A-HJKMNP-TV-Z]{26}$/;
// RFC 4122 v4 UUID: 8-4-4-4-12 hex with version nibble 4 and variant 8/9/a/b.
const UUID_V4_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

const ULID_GENERATORS = {
  session: newSessionId,
  message: newMessageId,
  attachment: newAttachmentId,
  client: newClientId,
  job: newJobId,
  stream: newStreamId,
  call: newCallId,
} as const;

for (const [name, gen] of Object.entries(ULID_GENERATORS)) {
  Deno.test(`new${name[0].toUpperCase()}${name.slice(1)}Id: matches ULID format`, () => {
    assertMatch(gen(), ULID_PATTERN);
  });
}

Deno.test("newRequestId: matches RFC 4122 v4 UUID", () => {
  assertMatch(newRequestId(), UUID_V4_PATTERN);
});

Deno.test("ULID ids: 10_000 calls produce 10_000 distinct values", () => {
  const N = 10_000;
  const seen = new Set<string>();
  for (let i = 0; i < N; i++) seen.add(newSessionId());
  assertEquals(seen.size, N);
});

Deno.test("UUID ids: 10_000 calls produce 10_000 distinct values", () => {
  const N = 10_000;
  const seen = new Set<string>();
  for (let i = 0; i < N; i++) seen.add(newRequestId());
  assertEquals(seen.size, N);
});

Deno.test("session and message ids draw from the same ULID stream but never collide", () => {
  // Sanity check that the wrappers don't accidentally return a shared
  // singleton or memoized value.
  assertNotEquals(newSessionId(), newMessageId());
  assertNotEquals(newSessionId(), newSessionId());
});
