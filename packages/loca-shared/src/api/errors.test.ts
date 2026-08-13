// every error code has an HTTP status. Tested as a single pass so adding
// a new ERROR_CODES entry without ERROR_STATUS coverage trips the suite.

import { assertEquals } from "@std/assert";
import { ERROR_CODES, ERROR_STATUS, type ErrorCode, isApiErrorBody } from "./errors.ts";

Deno.test("ERROR_STATUS: every ErrorCode has a status, every status is a 4xx/5xx", () => {
  for (const code of ERROR_CODES) {
    const status = ERROR_STATUS[code];
    assertEquals(typeof status, "number", `${code} missing status`);
    assertEquals(status >= 400 && status < 600, true, `${code} out of range`);
  }
});

Deno.test("ERROR_STATUS: no extra keys beyond ERROR_CODES", () => {
  assertEquals(Object.keys(ERROR_STATUS).sort(), [...ERROR_CODES].sort());
});

Deno.test("isApiErrorBody: true for valid wire-format envelopes", () => {
  const body = {
    error: { code: "not_found" satisfies ErrorCode, message: "x" },
  };
  assertEquals(isApiErrorBody(body), true);
});

Deno.test("isApiErrorBody: rejects malformed and unknown-code envelopes", () => {
  for (const v of [
    null,
    undefined,
    "string",
    { error: null },
    { error: {} },
    { error: { code: "definitely-not-a-real-code", message: "" } },
  ]) {
    assertEquals(isApiErrorBody(v), false, `should reject ${JSON.stringify(v)}`);
  }
});
