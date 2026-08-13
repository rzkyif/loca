import { assert, assertEquals, assertThrows } from "@std/assert";
import type { Tool } from "@loca/shared";
import { __resetValidatorCacheForTesting, validateAndNormalizeToolArgs } from "./validate-args.ts";
import { AppError } from "@loca/core-engine";

function tool(parameters: Record<string, unknown>, id = "tk::t"): Tool {
  return {
    id,
    extensionId: "tk",
    name: "t",
    description: "",
    parameters,
    triggers: [],
    fnExport: "t",
    alwaysAvailable: false,
    platforms: [],
    enabled: true,
    requiredPermissions: [],
    missingRequired: [],
    grants: [],
  };
}

Deno.test("validateAndNormalizeToolArgs: fills schema defaults", () => {
  __resetValidatorCacheForTesting();
  const t = tool({
    type: "object",
    properties: {
      unit: { type: "string", default: "celsius" },
      city: { type: "string" },
    },
    required: ["city"],
  });
  assertEquals(JSON.parse(validateAndNormalizeToolArgs(t, JSON.stringify({ city: "Paris" }))), {
    city: "Paris",
    unit: "celsius",
  });
});

Deno.test("validateAndNormalizeToolArgs: rejects a missing required property", () => {
  __resetValidatorCacheForTesting();
  const t = tool({
    type: "object",
    properties: { city: { type: "string" } },
    required: ["city"],
  });
  assertThrows(() => validateAndNormalizeToolArgs(t, "{}"), AppError, "invalid arguments");
});

Deno.test("validateAndNormalizeToolArgs: rejects non-JSON arguments", () => {
  __resetValidatorCacheForTesting();
  assertThrows(
    () => validateAndNormalizeToolArgs(tool({ type: "object" }), "{not json"),
    AppError,
    "not valid JSON",
  );
});

Deno.test("validateAndNormalizeToolArgs: empty args become an empty object (with defaults)", () => {
  __resetValidatorCacheForTesting();
  const t = tool({
    type: "object",
    properties: { x: { type: "number", default: 5 } },
  });
  assertEquals(JSON.parse(validateAndNormalizeToolArgs(t, "")), { x: 5 });
});

Deno.test("validateAndNormalizeToolArgs: recompiles when the schema changes for the same tool id", () => {
  __resetValidatorCacheForTesting();
  const id = "tk::t";
  assertEquals(
    JSON.parse(
      validateAndNormalizeToolArgs(
        tool(
          {
            type: "object",
            properties: { a: { type: "string", default: "x" } },
          },
          id,
        ),
        "{}",
      ),
    ),
    { a: "x" },
  );
  // Same id, different schema must not reuse the stale validator.
  assertEquals(
    JSON.parse(
      validateAndNormalizeToolArgs(
        tool(
          {
            type: "object",
            properties: { b: { type: "string", default: "y" } },
          },
          id,
        ),
        "{}",
      ),
    ),
    { b: "y" },
  );
});

Deno.test("validateAndNormalizeToolArgs: an unusable schema passes args through unchanged", () => {
  __resetValidatorCacheForTesting();
  // `type: "nonsense"` is not a valid JSON-Schema type; compile fails and we
  // fall back to passing the raw args rather than wedging the call.
  const raw = JSON.stringify({ anything: true });
  assertEquals(validateAndNormalizeToolArgs(tool({ type: "nonsense" }), raw), raw);
});

Deno.test("validateAndNormalizeToolArgs: a catastrophic-backtracking author pattern is stripped, not run (ReDoS guard)", () => {
  __resetValidatorCacheForTesting();
  // `(a+)+$` is a classic ReDoS pattern: against this input a backtracking
  // engine explores ~2^n paths and pins the event loop. The guard strips the
  // unsafe pattern before compiling, so validation stays bounded and the value
  // passes through unchecked rather than hanging the core.
  const t = tool({
    type: "object",
    properties: { s: { type: "string", pattern: "(a+)+$" } },
  });
  const evil = "a".repeat(40) + "!";
  const start = performance.now();
  const out = JSON.parse(validateAndNormalizeToolArgs(t, JSON.stringify({ s: evil })));
  const elapsedMs = performance.now() - start;
  assertEquals(out, { s: evil });
  assert(elapsedMs < 1000, `validation took ${elapsedMs}ms; the unsafe pattern was not stripped`);
});

Deno.test("validateAndNormalizeToolArgs: an unsafe patternProperties key is stripped (ReDoS guard)", () => {
  __resetValidatorCacheForTesting();
  const t = tool({
    type: "object",
    patternProperties: { "(a+)+$": { type: "string" } },
  });
  const evil = "a".repeat(40) + "!";
  const start = performance.now();
  // The pathological key would trigger the unsafe regex during property
  // matching; with the key stripped, validation is bounded.
  const out = JSON.parse(validateAndNormalizeToolArgs(t, JSON.stringify({ [evil]: "x" })));
  const elapsedMs = performance.now() - start;
  assertEquals(out, { [evil]: "x" });
  assert(elapsedMs < 1000, `validation took ${elapsedMs}ms; the unsafe pattern was not stripped`);
});

Deno.test("validateAndNormalizeToolArgs: a safe author pattern is still enforced", () => {
  __resetValidatorCacheForTesting();
  const t = tool({
    type: "object",
    properties: { code: { type: "string", pattern: "^[0-9]{3}$" } },
    required: ["code"],
  });
  assertEquals(JSON.parse(validateAndNormalizeToolArgs(t, JSON.stringify({ code: "123" }))), {
    code: "123",
  });
  assertThrows(
    () => validateAndNormalizeToolArgs(t, JSON.stringify({ code: "abc" })),
    AppError,
    "invalid arguments",
  );
});
