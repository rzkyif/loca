// AppError construction, type guard, and the disk-full classifier used by
// the HTTP error middleware to translate ENOSPC into a 507.

import { assertEquals, assertStrictEquals } from "@std/assert";
import { ERROR_STATUS } from "@loca/shared";
import {
  AppError,
  conflict,
  forbidden,
  internal,
  isAppError,
  isNoSpaceError,
  notFound,
  validation,
} from "@loca/core-engine";

Deno.test("AppError: maps every ErrorCode to its documented HTTP status", () => {
  for (const [code, status] of Object.entries(ERROR_STATUS)) {
    const err = new AppError(code as keyof typeof ERROR_STATUS, "msg");
    assertEquals(err.code, code);
    assertEquals(err.status, status);
    assertEquals(err.name, "AppError");
    assertEquals(err.message, "msg");
  }
});

Deno.test("AppError: preserves details payload", () => {
  const details = { resource: "session", id: "abc" };
  const err = new AppError("not_found", "missing", details);
  assertEquals(err.details, details);
});

Deno.test("isAppError: true only for AppError instances", () => {
  assertStrictEquals(isAppError(new AppError("internal_error", "x")), true);
  assertStrictEquals(isAppError(new Error("plain")), false);
  assertStrictEquals(isAppError("string"), false);
  assertStrictEquals(isAppError(null), false);
  assertStrictEquals(isAppError(undefined), false);
  assertStrictEquals(isAppError({ code: "not_found" }), false);
});

Deno.test("helper throwers: each tags the right code and re-uses the message", () => {
  for (const [thrower, code] of [
    [notFound, "not_found"],
    [validation, "validation_error"],
    [conflict, "conflict"],
    [forbidden, "forbidden"],
    [internal, "internal_error"],
  ] as const) {
    try {
      thrower("boom");
    } catch (err) {
      assertEquals(isAppError(err), true);
      const app = err as AppError;
      assertEquals(app.code, code);
      assertEquals(app.message, "boom");
    }
  }
});

Deno.test("isNoSpaceError: matches ENOSPC on POSIX", () => {
  const err = Object.assign(new Error("write failed"), { code: "ENOSPC" });
  assertEquals(isNoSpaceError(err), true);
});

Deno.test("isNoSpaceError: matches Windows ERROR_DISK_FULL", () => {
  const err = Object.assign(new Error("write failed"), {
    code: "ERROR_DISK_FULL",
  });
  assertEquals(isNoSpaceError(err), true);
});

Deno.test("isNoSpaceError: matches by message when error has no code", () => {
  for (const msg of [
    "No space left on device",
    "disk full while flushing",
    "Quota exceeded for current user",
  ]) {
    assertEquals(isNoSpaceError(new Error(msg)), true, `should match: ${msg}`);
  }
});

Deno.test("isNoSpaceError: rejects non-Error values and unrelated errors", () => {
  assertEquals(isNoSpaceError(new Error("permission denied")), false);
  assertEquals(isNoSpaceError("ENOSPC"), false);
  assertEquals(isNoSpaceError({ code: "ENOSPC" }), false);
  assertEquals(isNoSpaceError(null), false);
});
