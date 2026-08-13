// Unit coverage for the add-a-core wizard's state machine (NewCoreWizard):
// the first-boot routing (silent pair with an installer-provided local core
// vs the welcoming remote-connect form), the connect-local flow itself, and
// the remote probe + pair flow. These tests stub the platform + the $lib/core
// pairing free functions and drive the class directly.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { type Platform, setPlatform } from "$lib/platform";

// Hoisted spies so the vi.mock factories (which are hoisted above imports) can
// close over them. The wizard imports the pairing free functions + cores() from
// $lib/core, viewState/modelRecommendState from $stores, and getLogger from
// log; all are replaced here so the class is tested in isolation.
const h = vi.hoisted(() => ({
  cores: {
    list: vi.fn<() => Promise<Array<{ baseUrl: string }>>>(),
    addPaired: vi.fn<(entry: unknown, token: string) => Promise<void>>(),
    select: vi.fn<(id: string) => Promise<void>>(),
  },
  mintCodeWithAdminToken: vi.fn(),
  pairWithCode: vi.fn(),
  probeCore: vi.fn(),
  viewState: {
    setLocked: vi.fn<(v: boolean) => void>(),
    navigate: vi.fn<(m: string) => void>(),
    pendingSettingsGroup: null as string | null,
  },
  modelRecommendState: { applyBucket: vi.fn<() => Promise<void>>() },
}));

vi.mock("$lib/core", () => ({
  cores: () => h.cores,
  isLoopbackUrl: (u: string) => u.includes("127.0.0.1") || u.includes("localhost"),
  mintCodeWithAdminToken: h.mintCodeWithAdminToken,
  pairWithCode: h.pairWithCode,
  probeCore: h.probeCore,
}));
vi.mock("$stores", () => ({
  viewState: h.viewState,
  modelRecommendState: h.modelRecommendState,
}));
vi.mock("$lib/util/log", () => ({
  getLogger: () => ({ warn: vi.fn(), error: vi.fn(), info: vi.fn(), debug: vi.fn() }),
}));

// Imported after the mocks are declared (vi.mock is hoisted regardless).
import { hostFromUrl, NewCoreWizard } from "./use-new-core-wizard.svelte";

const LOCAL_URL = "https://127.0.0.1:7800";
const PAIR_RESULT = {
  token: "tok",
  clientId: "client-1",
  coreVersion: "1.2.3",
  trustMode: "pin" as const,
  tlsPin: "PIN",
};

// A fresh mock platform each test. Only the namespaces the wizard touches are
// populated; tests overwrite individual spies to drive specific branches. The
// inferred shape keeps the per-spy `.mockResolvedValue` overrides typed.
function makePlatform() {
  return {
    pairing: {
      readAdminToken: vi.fn<() => Promise<string | null>>(async () => "admin-token"),
      readLocalCoreBootError: vi.fn<() => Promise<string | null>>(async () => null),
      isLocalCoreInstalled: vi.fn(async () => false),
      startLocalCore: vi.fn(async () => true),
      localCoreBaseUrl: vi.fn(async () => LOCAL_URL),
      localSidecarPorts: vi.fn(async () => ({ llm: 7701, stt: 7702 })),
      launchPrefill: vi.fn<() => Promise<{ coreUrl?: string; pairingCode?: string } | null>>(
        async () => null,
      ),
    },
    net: { discoverCores: vi.fn(async () => []) },
  };
}

let plat: ReturnType<typeof makePlatform>;

beforeEach(() => {
  vi.clearAllMocks();
  plat = makePlatform();
  setPlatform(plat as unknown as Platform);
  h.cores.list.mockResolvedValue([]);
  h.cores.addPaired.mockResolvedValue(undefined);
  h.cores.select.mockResolvedValue(undefined);
  h.mintCodeWithAdminToken.mockResolvedValue({ code: "654321" });
  h.pairWithCode.mockResolvedValue(PAIR_RESULT);
  h.modelRecommendState.applyBucket.mockResolvedValue(undefined);
  h.viewState.pendingSettingsGroup = null;
});

describe("hostFromUrl", () => {
  it("returns the host for a valid URL", () => {
    expect(hostFromUrl("https://a.b.com:7800/path")).toBe("a.b.com:7800");
  });
  it("falls back to a label for a non-URL", () => {
    expect(hostFromUrl("not a url")).toBe("Remote Core");
  });
});

describe("getters", () => {
  it("canStepBack is only available on the remote-pair step", () => {
    const w = new NewCoreWizard(false);
    w.view = "remotePair";
    expect(w.canStepBack).toBe(true);
    w.view = "remoteAddress";
    expect(w.canStepBack).toBe(false);
    w.view = "connectLocal";
    expect(w.canStepBack).toBe(false);
  });

  it("normalizedRemoteUrl coerces to https and trims", () => {
    const w = new NewCoreWizard(false);
    w.remoteUrl = "http://x:7800";
    expect(w.normalizedRemoteUrl()).toBe("https://x:7800");
    w.remoteUrl = "x:7800";
    expect(w.normalizedRemoteUrl()).toBe("https://x:7800");
    w.remoteUrl = "https://x:7800/";
    expect(w.normalizedRemoteUrl()).toBe("https://x:7800");
    w.remoteUrl = "   ";
    expect(w.normalizedRemoteUrl()).toBe("");
  });
});

describe("decideInitialView", () => {
  it("mobile goes straight to the remote form without probing for a local core", async () => {
    const w = new NewCoreWizard(true);
    await w.decideInitialView();
    expect(w.view).toBe("remoteAddress");
    expect(plat.pairing.isLocalCoreInstalled).not.toHaveBeenCalled();
  });

  it("desktop without an installed local core opens the remote form", async () => {
    const w = new NewCoreWizard(false);
    await w.decideInitialView();
    expect(w.view).toBe("remoteAddress");
    expect(plat.pairing.isLocalCoreInstalled).toHaveBeenCalled();
  });

  it("desktop with an installer-provided local core silently pairs with it", async () => {
    plat.pairing.isLocalCoreInstalled.mockResolvedValue(true);
    const w = new NewCoreWizard(false);
    await w.decideInitialView();
    expect(w.view).toBe("connectLocal");
    // The auto-run pair: start the core, mint via the admin token, claim.
    await vi.waitFor(() => expect(h.pairWithCode).toHaveBeenCalled());
    expect(plat.pairing.startLocalCore).toHaveBeenCalled();
    expect(h.mintCodeWithAdminToken).toHaveBeenCalledWith(LOCAL_URL, "admin-token");
    expect(h.pairWithCode).toHaveBeenCalledWith(LOCAL_URL, "Loca Client", "654321", false);
  });

  it("skips the local probe when a local core is already paired", async () => {
    h.cores.list.mockResolvedValue([{ baseUrl: LOCAL_URL }]);
    plat.pairing.isLocalCoreInstalled.mockResolvedValue(true);
    const w = new NewCoreWizard(false);
    await w.decideInitialView();
    expect(w.view).toBe("remoteAddress");
    expect(plat.pairing.isLocalCoreInstalled).not.toHaveBeenCalled();
  });

  it("adding another core opens the remote form even with an unpaired local core on disk", async () => {
    // The silent local pair is a first-boot-only affordance: once ANY core is
    // paired, the wizard entry is "add another core" and must never auto-pair.
    h.cores.list.mockResolvedValue([{ baseUrl: "https://192.168.1.5:7800" }]);
    plat.pairing.isLocalCoreInstalled.mockResolvedValue(true);
    const w = new NewCoreWizard(false);
    await w.decideInitialView();
    expect(w.view).toBe("remoteAddress");
    expect(plat.pairing.isLocalCoreInstalled).not.toHaveBeenCalled();
    expect(h.pairWithCode).not.toHaveBeenCalled();
  });

  it("prefills empty remote fields from launch arguments", async () => {
    plat.pairing.launchPrefill.mockResolvedValue({
      coreUrl: "https://prefill:7800",
      pairingCode: "111111",
    });
    const w = new NewCoreWizard(false);
    await w.decideInitialView();
    expect(w.remoteUrl).toBe("https://prefill:7800");
    expect(w.remoteCode).toBe("111111");
  });

  it("does not clobber remote fields the user already typed", async () => {
    plat.pairing.launchPrefill.mockResolvedValue({
      coreUrl: "https://prefill:7800",
      pairingCode: "111111",
    });
    const w = new NewCoreWizard(false);
    w.remoteUrl = "https://typed:7800";
    w.remoteCode = "999999";
    await w.decideInitialView();
    expect(w.remoteUrl).toBe("https://typed:7800");
    expect(w.remoteCode).toBe("999999");
  });
});

describe("connectLocal", () => {
  it("pairs, applies the hardware-fit preset, unlocks, and opens quick settings", async () => {
    const w = new NewCoreWizard(false);
    await w.connectLocal();

    expect(plat.pairing.startLocalCore).toHaveBeenCalled();
    expect(h.mintCodeWithAdminToken).toHaveBeenCalledWith(LOCAL_URL, "admin-token");
    expect(h.pairWithCode).toHaveBeenCalledWith(LOCAL_URL, "Loca Client", "654321", false);
    const [entry, token] = h.cores.addPaired.mock.calls[0];
    expect(entry).toMatchObject({ name: "Local Core", baseUrl: LOCAL_URL, trustMode: "pin" });
    expect(token).toBe("tok");
    expect(h.cores.select).toHaveBeenCalledWith("client-1");
    // First core ever: hardware-fit preset applied, UI unlocked, quick settings.
    expect(h.modelRecommendState.applyBucket).toHaveBeenCalledWith("smallest");
    expect(h.viewState.setLocked).toHaveBeenCalledWith(false);
    expect(h.viewState.navigate).toHaveBeenCalledWith("quickSettings");
    expect(w.busy).toBeNull();
    expect(w.error).toBe("");
  });

  it("resolves the channel-aware base URL before minting", async () => {
    plat.pairing.localCoreBaseUrl.mockResolvedValue("https://127.0.0.1:7810");
    const w = new NewCoreWizard(false);
    await w.connectLocal();
    expect(h.mintCodeWithAdminToken).toHaveBeenCalledWith("https://127.0.0.1:7810", "admin-token");
  });

  it("routes an additional core back to the Cores settings manager", async () => {
    h.cores.list.mockResolvedValue([{ baseUrl: "https://192.168.1.9:7800" }]);
    const w = new NewCoreWizard(false);
    await w.connectLocal();
    expect(h.modelRecommendState.applyBucket).not.toHaveBeenCalled();
    expect(h.viewState.setLocked).not.toHaveBeenCalled();
    expect(h.viewState.pendingSettingsGroup).toBe("cores");
    expect(h.viewState.navigate).toHaveBeenCalledWith("settings");
  });

  it("errors clearly when the on-disk admin token is missing", async () => {
    plat.pairing.readAdminToken.mockResolvedValue(null);
    const w = new NewCoreWizard(false);
    await w.connectLocal();
    expect(w.error).toMatch(/admin token not found/i);
    expect(h.mintCodeWithAdminToken).not.toHaveBeenCalled();
    expect(w.busy).toBeNull();
  });

  it("errors when the mint response carries no code", async () => {
    h.mintCodeWithAdminToken.mockResolvedValue({ code: "" });
    const w = new NewCoreWizard(false);
    await w.connectLocal();
    expect(w.error).toMatch(/missing pairing code/i);
    expect(h.pairWithCode).not.toHaveBeenCalled();
    expect(w.busy).toBeNull();
  });

  it("folds the Core's boot-error breadcrumb into a failure message", async () => {
    plat.pairing.startLocalCore.mockRejectedValue(new Error("could not reach the core"));
    plat.pairing.readLocalCoreBootError.mockResolvedValue("port 7800 already in use");
    const w = new NewCoreWizard(false);
    await w.connectLocal();
    expect(w.error).toContain("could not reach the core");
    expect(w.error).toContain("port 7800 already in use");
    expect(w.busy).toBeNull();
  });

  it("useRemoteInstead falls back to the remote form and clears the error", async () => {
    plat.pairing.readAdminToken.mockResolvedValue(null);
    const w = new NewCoreWizard(false);
    w.view = "connectLocal";
    await w.connectLocal();
    expect(w.error).not.toBe("");
    w.useRemoteInstead();
    expect(w.view).toBe("remoteAddress");
    expect(w.error).toBe("");
  });
});

describe("remote flow", () => {
  it("checkConnection advances to the pair step on a healthy probe", async () => {
    h.probeCore.mockResolvedValue({ version: "1.2.3", behindProxy: false });
    const w = new NewCoreWizard(false);
    w.remoteUrl = "https://192.168.1.5:7800";
    await w.checkConnection();
    expect(w.view).toBe("remotePair");
    expect(w.connectionStatus).toMatchObject({ kind: "ok", version: "1.2.3" });
  });

  it("checkConnection surfaces a probe failure without advancing", async () => {
    h.probeCore.mockRejectedValue(new Error("timed out"));
    const w = new NewCoreWizard(false);
    w.view = "remoteAddress";
    w.remoteUrl = "https://192.168.1.5:7800";
    await w.checkConnection();
    expect(w.view).toBe("remoteAddress");
    expect(w.connectionStatus).toMatchObject({ kind: "error" });
  });

  it("pairRemote claims with the probed behindProxy trust mode", async () => {
    h.probeCore.mockResolvedValue({ version: "1.2.3", behindProxy: true });
    const w = new NewCoreWizard(false);
    w.remoteUrl = "https://core.example.com";
    await w.checkConnection();
    w.remoteCode = "123456";
    await w.pairRemote();
    expect(h.pairWithCode).toHaveBeenCalledWith(
      "https://core.example.com",
      "Loca Client",
      "123456",
      true,
    );
  });

  it("pairRemote refuses to claim before a matching connection check", async () => {
    const w = new NewCoreWizard(false);
    w.remoteUrl = "https://core.example.com";
    w.remoteCode = "123456";
    await w.pairRemote();
    expect(w.error).toMatch(/check the connection/i);
    expect(h.pairWithCode).not.toHaveBeenCalled();
  });
});

describe("navigation helpers", () => {
  it("goBack steps remotePair -> remoteAddress and clears the error", () => {
    const w = new NewCoreWizard(false);
    w.view = "remotePair";
    w.error = "boom";
    w.goBack();
    expect(w.view).toBe("remoteAddress");
    expect(w.error).toBe("");
  });

  it("goBack is inert while busy", () => {
    const w = new NewCoreWizard(false);
    w.view = "remotePair";
    w.busy = "claiming";
    w.goBack();
    expect(w.view).toBe("remotePair");
  });

  it("exitFlow returns to the Cores settings manager", () => {
    const w = new NewCoreWizard(false);
    w.exitFlow();
    expect(h.viewState.pendingSettingsGroup).toBe("cores");
    expect(h.viewState.navigate).toHaveBeenCalledWith("settings");
  });
});
