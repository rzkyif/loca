// The capabilities store's job is to answer "can the CORE do this", and to
// answer it safely before it knows. Two invariants are under test: unknown is
// treated as capable (a false negative hides a working feature with no way to
// discover it, where a false positive shows a control that fails with a worded
// reason), and one core's answer never gates another core's UI.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ConnectionListener } from "$lib/core/client";
import type { CoreCapabilities } from "@loca/shared";

let connListener: ConnectionListener | null = null;
let registryListener: (() => void) | null = null;
let currentId: string | null = "core-a";
let health: { capabilities?: CoreCapabilities } = {};
let getCalls = 0;

vi.mock("$lib/core", () => ({
  cores: () => ({
    currentEntry: () => (currentId === null ? null : { id: currentId }),
    currentClient: () => ({
      get: (_path: string) => {
        getCalls++;
        return Promise.resolve(health);
      },
    }),
    subscribeConnectionState: (l: ConnectionListener) => {
      connListener = l;
      return () => {
        connListener = null;
      };
    },
    subscribe: (l: () => void) => {
      registryListener = l;
      return () => {
        registryListener = null;
      };
    },
  }),
}));

const { capabilitiesState } = await import("./capabilities.svelte");

const ALL_OFF: CoreCapabilities = {
  extensions: false,
  localInference: false,
  localSpeech: false,
  scheduledPrompts: false,
  stdioMcp: false,
  pairing: false,
  coreUpdate: false,
};

async function connect(): Promise<void> {
  if (!connListener) throw new Error("store is not attached");
  connListener("connected");
  // The fetch is a microtask; let it settle.
  await Promise.resolve();
  await Promise.resolve();
}

describe("capabilitiesState", () => {
  beforeEach(() => {
    currentId = "core-a";
    health = {};
    getCalls = 0;
    capabilitiesState.attach();
  });
  afterEach(() => capabilitiesState.detach());

  it("treats unknown as capable, so nothing is hidden before the first answer", () => {
    expect(capabilitiesState.current).toBe(null);
    expect(capabilitiesState.can("extensions")).toBe(true);
    expect(capabilitiesState.can("pairing")).toBe(true);
  });

  it("reports what the connected core actually said", async () => {
    health = { capabilities: { ...ALL_OFF, extensions: true } };
    await connect();
    expect(capabilitiesState.can("extensions")).toBe(true);
    expect(capabilitiesState.can("pairing")).toBe(false);
    expect(capabilitiesState.can("stdioMcp")).toBe(false);
  });

  it("stays capable against a core that sends no capabilities block", async () => {
    // The shipping Deno core before it grew one, and any core that predates
    // negotiation. A pessimistic default would blank half its settings panel.
    health = {};
    await connect();
    expect(capabilitiesState.can("extensions")).toBe(true);
    expect(capabilitiesState.can("localInference")).toBe(true);
  });

  it("never fetches until the core is connected", () => {
    expect(getCalls).toBe(0);
    if (!connListener) throw new Error("store is not attached");
    connListener("connecting");
    connListener("disconnected");
    connListener("gaveUp");
    // The absolute rule: no outbound request without a user action, and no
    // background poll. Only a live connection triggers one.
    expect(getCalls).toBe(0);
  });

  it("forgets the previous core's answer on a switch", async () => {
    health = { capabilities: ALL_OFF };
    await connect();
    expect(capabilitiesState.can("extensions")).toBe(false);

    currentId = "core-b";
    registryListener?.();
    // Back to unknown, not to the old core's answer: a desktop core reached
    // after an embedded one must not inherit "no extensions".
    expect(capabilitiesState.current).toBe(null);
    expect(capabilitiesState.can("extensions")).toBe(true);
  });

  it("ignores a response that arrives after the user switched cores", async () => {
    health = { capabilities: ALL_OFF };
    if (!connListener) throw new Error("store is not attached");
    connListener("connected");
    // The switch lands while the health request is still in flight.
    currentId = "core-b";
    registryListener?.();
    await Promise.resolve();
    await Promise.resolve();
    // Applying the in-flight answer here would gate core-b's UI on core-a's
    // capabilities, which is this store's own bug in miniature.
    expect(capabilitiesState.current).toBe(null);
    expect(capabilitiesState.can("extensions")).toBe(true);
  });

  it("leaves capability unknown when the probe fails", async () => {
    if (!connListener) throw new Error("store is not attached");
    health = null as unknown as { capabilities?: CoreCapabilities };
    connListener("connected");
    await Promise.resolve();
    await Promise.resolve();
    // A failed probe is a transport problem, not evidence about the core.
    expect(capabilitiesState.current).toBe(null);
    expect(capabilitiesState.can("extensions")).toBe(true);
  });
});
