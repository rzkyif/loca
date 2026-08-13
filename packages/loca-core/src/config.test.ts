// boot config env override + port validation. Pure logic: reads
// Deno.env but doesn't write anything else.

import { assertEquals, assertThrows } from "@std/assert";
import { CORE_VERSION, DEFAULT_HOST, DEFAULT_PORT, loadBootConfig } from "./config.ts";

function withEnv(patch: Record<string, string | undefined>, fn: () => void): void {
  const prior: Record<string, string | undefined> = {};
  for (const k of Object.keys(patch)) prior[k] = Deno.env.get(k);
  try {
    for (const [k, v] of Object.entries(patch)) {
      if (v === undefined) Deno.env.delete(k);
      else Deno.env.set(k, v);
    }
    fn();
  } finally {
    for (const [k, v] of Object.entries(prior)) {
      if (v === undefined) Deno.env.delete(k);
      else Deno.env.set(k, v);
    }
  }
}

Deno.test("loadBootConfig: defaults when no env vars are set", () => {
  withEnv({ LOCA_CORE_HOST: undefined, LOCA_CORE_PORT: undefined }, () => {
    const cfg = loadBootConfig();
    assertEquals(cfg.host, DEFAULT_HOST);
    assertEquals(cfg.port, DEFAULT_PORT);
    assertEquals(cfg.version, CORE_VERSION);
  });
});

Deno.test("loadBootConfig: honors LOCA_CORE_HOST and LOCA_CORE_PORT", () => {
  withEnv({ LOCA_CORE_HOST: "0.0.0.0", LOCA_CORE_PORT: "9000" }, () => {
    const cfg = loadBootConfig();
    assertEquals(cfg.host, "0.0.0.0");
    assertEquals(cfg.port, 9000);
  });
});

Deno.test("loadBootConfig: rejects non-numeric port", () => {
  withEnv({ LOCA_CORE_PORT: "abc" }, () => {
    assertThrows(() => loadBootConfig(), Error, "invalid LOCA_CORE_PORT");
  });
});

Deno.test("loadBootConfig: rejects out-of-range ports", () => {
  for (const port of ["0", "65536", "-1", "1.5"]) {
    withEnv({ LOCA_CORE_PORT: port }, () => {
      assertThrows(() => loadBootConfig(), Error, "invalid LOCA_CORE_PORT");
    });
  }
});
