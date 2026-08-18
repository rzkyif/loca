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
    list: vi.fn<() => Promise<Array<{ id?: string; baseUrl: string }>>>(),
    addPaired: vi.fn<(entry: unknown, token: string) => Promise<void>>(),
    adoptLocalCore: vi.fn<(name: string) => Promise<void>>(),
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
  isLocalCoreEntry: (e: { id?: string }) => e.id === "local",
  isLoopbackUrl: (u: string) => u.includes("127.0.0.1") || u.includes("localhost"),
  LOCAL_CORE_ID: "local",
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
    // Null by default = a build with no Core inside it (every desktop build).
    // Tests that want the fork set this to a marker; the wizard only ever asks
    // whether it is there, never calls it.
    localCore: null as unknown,
  };
}

/** A build that carries a Core of its own. */
function withEmbeddedCore(): void {
  plat.localCore = {};
}

let plat: ReturnType<typeof makePlatform>;

beforeEach(() => {
  vi.clearAllMocks();
  plat = makePlatform();
  setPlatform(plat as unknown as Platform);
  h.cores.list.mockResolvedValue([]);
  h.cores.addPaired.mockResolvedValue(undefined);
  h.cores.adoptLocalCore.mockResolvedValue(undefined);
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

  it("the address step steps back to the fork only when the fork was shown", () => {
    const w = new NewCoreWizard(true);
    w.view = "remoteAddress";
    expect(w.canStepBack).toBe(false);
    w.canRunHere = true;
    expect(w.canStepBack).toBe(true);
    // The fork itself is the first step, so it has nowhere to go back to.
    w.view = "choose";
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
  it("mobile with no Core inside the build goes straight to the remote form", async () => {
    const w = new NewCoreWizard(true);
    await w.decideInitialView();
    expect(w.view).toBe("remoteAddress");
    expect(w.canRunHere).toBe(false);
    expect(plat.pairing.isLocalCoreInstalled).not.toHaveBeenCalled();
  });

  it("a build that carries a Core of its own opens on the fork", async () => {
    withEmbeddedCore();
    const w = new NewCoreWizard(true);
    await w.decideInitialView();
    expect(w.view).toBe("choose");
    expect(w.canRunHere).toBe(true);
  });

  it("offers the fork on a desktop build too when it carries a Core", async () => {
    // The gate is a fact about the BUILD, not the form factor. A platform sniff
    // here would be the same bug capability negotiation exists to fix.
    withEmbeddedCore();
    const w = new NewCoreWizard(false);
    await w.decideInitialView();
    expect(w.view).toBe("choose");
  });

  it("does not offer the fork once this device's Core is already adopted", async () => {
    withEmbeddedCore();
    h.cores.list.mockResolvedValue([{ id: "local", baseUrl: "https://local.loca.invalid" }]);
    const w = new NewCoreWizard(true);
    await w.decideInitialView();
    expect(w.canRunHere).toBe(false);
    expect(w.view).toBe("remoteAddress");
  });

  it("still offers the fork when adding another core, if this device's is not adopted", async () => {
    withEmbeddedCore();
    h.cores.list.mockResolvedValue([{ id: "remote-1", baseUrl: "https://192.168.1.5:7800" }]);
    const w = new NewCoreWizard(true);
    await w.decideInitialView();
    expect(w.view).toBe("choose");
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

describe("the fork's two branches", () => {
  beforeEach(() => {
    withEmbeddedCore();
  });

  it("useThisDevice adopts and selects WITHOUT pairing or a keychain write", async () => {
    // The whole point of the branch: the Core is already this device and the
    // client it serves is pre-authenticated, so there is no code to mint, no
    // PAKE to run and no token to keep.
    const w = new NewCoreWizard(true);
    await w.useThisDevice();
    expect(h.cores.adoptLocalCore).toHaveBeenCalledTimes(1);
    expect(h.cores.select).toHaveBeenCalledWith("local");
    expect(h.pairWithCode).not.toHaveBeenCalled();
    expect(h.mintCodeWithAdminToken).not.toHaveBeenCalled();
    expect(h.cores.addPaired).not.toHaveBeenCalled();
    expect(plat.pairing.readAdminToken).not.toHaveBeenCalled();
  });

  it("records it under the name the copy ruling settled on", async () => {
    // "This Device", not "Local Core". The phrase took a ruling of its own
    // (Q4, against COPY.md:68, which governs where a MODEL runs) and it is both
    // user-visible and persisted into cores.json, so it is pinned rather than
    // left to drift.
    const w = new NewCoreWizard(true);
    await w.useThisDevice();
    expect(h.cores.adoptLocalCore).toHaveBeenCalledWith("This Device");
  });

  it("warms the embedded Core BEFORE selecting it, so the first connect is not a cold boot", async () => {
    // select() rebinds every persistent WS listener onto the new client, which
    // connects synchronously. On a fresh install that connect is the Core's
    // first-ever boot (root dirs, SQLite migrations, the Ed25519-verified
    // offline install of the built-in extension), and the client's connect
    // deadline would be measuring it. Warming first makes the race absent
    // rather than narrower.
    const order: string[] = [];
    plat.pairing.startLocalCore.mockImplementation(async () => {
      order.push("start");
      return true;
    });
    h.cores.adoptLocalCore.mockImplementation(async () => {
      order.push("adopt");
    });
    h.cores.select.mockImplementation(async () => {
      order.push("select");
    });
    const w = new NewCoreWizard(true);
    await w.useThisDevice();
    expect(order).toEqual(["start", "adopt", "select"]);
  });

  it("adopts nothing when the embedded Core will not start", async () => {
    // Warming first is also what keeps a Core that cannot boot out of the
    // registry: the user stays on the fork with the reason, rather than being
    // navigated into a chat view behind a connection that can never open.
    plat.pairing.startLocalCore.mockRejectedValue(new Error("no space left on device"));
    const w = new NewCoreWizard(true);
    await w.useThisDevice();
    expect(w.error).toContain("no space left on device");
    expect(h.cores.adoptLocalCore).not.toHaveBeenCalled();
    expect(h.cores.select).not.toHaveBeenCalled();
    expect(w.busy).toBeNull();
  });

  it("useThisDevice unlocks the UI and opens quick settings on the first core ever", async () => {
    // The adoption itself is what makes the registry non-empty, so "was this the
    // first core ever" has to be answered BEFORE it lands. Read afterwards, the
    // answer is always no and a first-run phone stays locked in the wizard it
    // just finished.
    // No `mockResolvedValueOnce([])` here: a queued first answer would be handed
    // to whichever call came first and would satisfy the mutation too, which is
    // how this assertion passed while proving nothing. The registry starts empty
    // (beforeEach) and the adoption is what fills it.
    h.cores.adoptLocalCore.mockImplementation(async () => {
      h.cores.list.mockResolvedValue([{ id: "local", baseUrl: "https://local.loca.invalid" }]);
    });
    const w = new NewCoreWizard(true);
    await w.useThisDevice();
    expect(h.viewState.setLocked).toHaveBeenCalledWith(false);
    expect(h.viewState.navigate).toHaveBeenCalledWith("quickSettings");
    expect(h.modelRecommendState.applyBucket).toHaveBeenCalledWith("smallest");
    expect(w.busy).toBeNull();
  });

  it("useThisDevice returns to the Cores manager when a core is already paired", async () => {
    h.cores.list.mockResolvedValue([{ id: "remote-1", baseUrl: "https://192.168.1.5:7800" }]);
    const w = new NewCoreWizard(true);
    await w.useThisDevice();
    expect(h.viewState.setLocked).not.toHaveBeenCalled();
    expect(h.viewState.pendingSettingsGroup).toBe("cores");
    expect(h.viewState.navigate).toHaveBeenCalledWith("settings");
    expect(h.modelRecommendState.applyBucket).not.toHaveBeenCalled();
  });

  it("useThisDevice surfaces a failed adoption and clears busy", async () => {
    h.cores.adoptLocalCore.mockRejectedValue(new Error("no Core inside this build"));
    const w = new NewCoreWizard(true);
    await w.useThisDevice();
    expect(w.error).toContain("no Core inside this build");
    expect(w.busy).toBeNull();
    expect(h.cores.select).not.toHaveBeenCalled();
  });

  it("useAnotherDevice moves on to the address form", () => {
    const w = new NewCoreWizard(true);
    w.view = "choose";
    w.error = "boom";
    w.useAnotherDevice();
    expect(w.view).toBe("remoteAddress");
    expect(w.error).toBe("");
  });

  it("goBack returns from the address form to the fork", async () => {
    const w = new NewCoreWizard(true);
    await w.decideInitialView();
    w.useAnotherDevice();
    w.goBack();
    expect(w.view).toBe("choose");
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
