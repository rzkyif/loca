import { assert, assertEquals, assertStrictEquals } from "@std/assert";
import { getLogger, initLogger } from "./log.ts";

Deno.test("getLogger: dependency scopes are gated to INFO, own scopes log DEBUG", async () => {
  await initLogger();
  assertEquals(getLogger("sidecars").levelName, "INFO");
  assertEquals(getLogger("logtest-own-scope").levelName, "DEBUG");
});

Deno.test("getLogger: scoped loggers are wired to the default handlers", async () => {
  // Mirrors module load order: scopes are requested at import time, before
  // main() runs initLogger(). Both must end up emitting through the same
  // handlers as the default logger, or scoped lines vanish silently.
  const before = getLogger("logtest-before-init");
  await initLogger();
  const after = getLogger("logtest-after-init");
  const def = getLogger();
  assert(def.handlers.length > 0);
  assertStrictEquals(before.handlers, def.handlers);
  assertStrictEquals(after.handlers, def.handlers);
});
