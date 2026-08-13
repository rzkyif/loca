// Client logger: dev-gating, extra stringify, and the pre-boot console
// fallback. `$app/environment` is mocked through a hoisted flag so we can flip
// `dev` between tests (the real shim hard-codes dev=true).

import { describe, expect, it, vi } from "vitest";

const env = vi.hoisted(() => ({ dev: true }));
vi.mock("$app/environment", () => ({
  get dev() {
    return env.dev;
  },
  get browser() {
    return true;
  },
}));

import { type Platform, setPlatform } from "$lib/platform";
import { getLogger } from "./log";

// Partial platform whose only job is to spy on logging.log. Mirrors the
// `as unknown as Platform` partial-mock pattern used in snippets.test.ts.
function spyPlatform(): ReturnType<typeof vi.fn> {
  const log = vi.fn();
  setPlatform({ logging: { log } } as unknown as Platform);
  return log;
}

describe("getLogger", () => {
  // MUST be first: runs before any setPlatform() so platform() throws and we
  // exercise the early-boot fallback (the module registry is fresh per file).
  it("falls back to console when platform() is unset, without throwing", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    expect(() => getLogger("boot").warn("early line")).not.toThrow();
    expect(warn).toHaveBeenCalledWith("[boot] early line");
    warn.mockRestore();
  });

  it("forwards all four levels in dev", () => {
    env.dev = true;
    const log = spyPlatform();
    const l = getLogger("ws");
    l.debug("d");
    l.info("i");
    l.warn("w");
    l.error("e");
    expect(log).toHaveBeenCalledTimes(4);
    expect(log).toHaveBeenCalledWith("debug", "ws", "d");
    expect(log).toHaveBeenCalledWith("error", "ws", "e");
  });

  it("suppresses debug/info in prod but still forwards warn/error", () => {
    env.dev = false;
    const log = spyPlatform();
    const l = getLogger("ws");
    l.debug("d");
    l.info("i");
    l.warn("w");
    l.error("e");
    expect(log).toHaveBeenCalledTimes(2);
    expect(log).toHaveBeenCalledWith("warn", "ws", "w");
    expect(log).toHaveBeenCalledWith("error", "ws", "e");
    env.dev = true;
  });

  it("appends an Error message and JSON-stringifies object context", () => {
    const log = spyPlatform();
    const l = getLogger("x");
    l.error("failed", new Error("boom"));
    expect(log).toHaveBeenCalledWith("error", "x", "failed boom");
    l.warn("ctx", { a: 1 });
    expect(log).toHaveBeenCalledWith("warn", "x", 'ctx {"a":1}');
  });

  it("does not throw when the sink itself throws", () => {
    const throwing = vi.fn(() => {
      throw new Error("ipc down");
    });
    setPlatform({ logging: { log: throwing } } as unknown as Platform);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    expect(() => getLogger("ws").warn("still safe")).not.toThrow();
    expect(warn).toHaveBeenCalledWith("[ws] still safe");
    warn.mockRestore();
  });
});
