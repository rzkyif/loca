// The mobile platform impl's answers about the Core it carries.
//
// Scoped to `localCore` and the `pairing` block, which is where mobile stopped
// being remote-only: the app links the real Core, so the desktop-shaped
// questions ("is one installed", "start it", "what is its URL") get the answers
// that are true of an embedded Core rather than the inert ones they used to
// return. Everything else in this file is shared with `tauri.ts` and covered by
// `tauri.test.ts`.
//
// `@tauri-apps/*` is mocked, with `__esModule: true` so vitest satisfies the ES
// module shape svelte-kit's bundler resolves.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
const listen = vi.fn((_event: string, _handler: unknown) => Promise.resolve(() => {}));

vi.mock("@tauri-apps/api/core", () => ({ __esModule: true, invoke }));
vi.mock("@tauri-apps/api/event", () => ({ __esModule: true, listen }));
vi.mock("@tauri-apps/api/app", () => ({
  __esModule: true,
  getVersion: vi.fn(() => Promise.resolve("1.2.3")),
  hide: vi.fn(() => Promise.resolve()),
  onBackButtonPress: vi.fn(() => Promise.resolve({ unregister: vi.fn() })),
}));
vi.mock("@tauri-apps/api/path", () => ({
  __esModule: true,
  join: vi.fn(),
  tempDir: vi.fn(() => Promise.resolve("/tmp")),
}));
vi.mock("@tauri-apps/plugin-fs", () => ({
  __esModule: true,
  readFile: vi.fn(),
  remove: vi.fn(),
  writeFile: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ __esModule: true, open: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({
  __esModule: true,
  openPath: vi.fn(),
  openUrl: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-os", () => ({ __esModule: true, platform: () => "android" }));
// The manifest's Ed25519 signature has its own coverage; these tests exercise
// the ABI SELECTION that happens after it, so the verifier is stubbed to accept
// rather than every case having to sign a fixture.
vi.mock("@loca/shared", async (orig) => ({
  ...(await orig<Record<string, unknown>>()),
  verifyEd25519Detached: () => true,
  // The APK hash check has its own meaning and its own coverage; these cases
  // are about WHICH APK is chosen, so the digest is stubbed to agree.
  sha256Hex: async () => "a".repeat(64),
  timingSafeEqualHex: () => true,
}));
vi.mock("$lib/menu/action-sheet-host.svelte", () => ({
  __esModule: true,
  presentActionSheet: vi.fn(),
}));

// Imported AFTER the mocks so the impl picks them up (a static import would be
// hoisted above the `const invoke = vi.fn()` the factories close over).
const { installMobilePlatform } = await import("./mobile");
const { platform } = await import("./index");

beforeEach(() => {
  vi.clearAllMocks();
  installMobilePlatform();
});

describe("the Core inside the mobile app", () => {
  it("is exposed, so lib/core can tell this build hosts one", () => {
    expect(platform().localCore).not.toBeNull();
  });

  it("reports itself installed: it is compiled in and cannot be missing", async () => {
    // The desktop answer is "is there a binary on disk"; here the Core ships
    // inside the app, so the honest answer is always yes. `false` would make the
    // registry prune it and the wizard hide it.
    await expect(platform().pairing.isLocalCoreInstalled()).resolves.toBe(true);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("starts and probes it through the core_* commands", async () => {
    invoke.mockResolvedValue({ version: "0.1.0", channel: "dev", root: "/data/core" });
    await expect(platform().pairing.startLocalCore()).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith("core_start");

    invoke.mockResolvedValue(true);
    await expect(platform().pairing.isLocalCoreRunning()).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith("core_running");
  });

  it("has no base URL, because it has no listener", async () => {
    await expect(platform().pairing.localCoreBaseUrl()).resolves.toBe("");
  });

  it("has no admin token and no boot breadcrumb: both belong to a supervised process", async () => {
    await expect(platform().pairing.readAdminToken()).resolves.toBeNull();
    await expect(platform().pairing.readLocalCoreBootError()).resolves.toBeNull();
  });

  it("stopping it is a no-op rather than a rejection", async () => {
    await expect(platform().pairing.stopLocalCore()).resolves.toBeUndefined();
  });

  it("marshals a request onto core_http", async () => {
    invoke.mockResolvedValue({ status: 200, headers: {}, bodyB64: "" });
    await platform().localCore?.http({
      url: "/api/v1/health",
      method: "GET",
      headers: { Authorization: "Bearer t" },
      bodyB64: null,
    });
    expect(invoke).toHaveBeenCalledWith("core_http", {
      url: "/api/v1/health",
      method: "GET",
      headers: { Authorization: "Bearer t" },
      bodyB64: null,
    });
  });

  it("drives the socket commands by id", async () => {
    await platform().localCore?.wsOpen("w1");
    await platform().localCore?.wsSend("w1", "frame");
    await platform().localCore?.wsClose("w1");
    expect(invoke).toHaveBeenCalledWith("core_ws_open", { wsId: "w1" });
    expect(invoke).toHaveBeenCalledWith("core_ws_send", { wsId: "w1", data: "frame" });
    expect(invoke).toHaveBeenCalledWith("core_ws_close", { wsId: "w1" });
  });

  it("subscribes to the three per-id events, and there is no error event", async () => {
    const handlers = { onOpen: vi.fn(), onMessage: vi.fn(), onClose: vi.fn() };
    const detach = await platform().localCore?.subscribeWs("w1", handlers);
    expect(listen.mock.calls.map((c) => c[0])).toEqual([
      "core://ws/w1/open",
      "core://ws/w1/message",
      "core://ws/w1/close",
    ]);
    expect(typeof detach).toBe("function");
  });

  // The one contract in this package that crosses the Rust/TypeScript boundary
  // with no shared constant. The names are formatted in Rust and typed again
  // here, so a rename on either side compiles, passes both suites, and produces
  // a phone that connects and then never receives a frame. Read the emitter and
  // compare, rather than restating the strings a third time.
  it("listens for exactly the event names the Rust side emits", async () => {
    // vitest's root is the package, so this resolves whichever task ran it.
    const src = readFileSync(resolve("src/tauri/src/commands/core.rs"), "utf8");
    const fmt = /format!\("([^"]*\{ws_id\}[^"]*\{kind\}[^"]*)"\)/.exec(src);
    if (!fmt) throw new Error("commands/core.rs no longer builds its event names as one format!");
    const kinds = [...src.matchAll(/ev\(ws_id, "([a-z]+)"\)/g)].map((m) => m[1]);
    expect(kinds).toEqual(["open", "message", "close"]);
    const emitted = kinds.map((k) => fmt[1].replace("{ws_id}", "w1").replace("{kind}", k));

    const handlers = { onOpen: vi.fn(), onMessage: vi.fn(), onClose: vi.fn() };
    await platform().localCore?.subscribeWs("w1", handlers);
    expect(listen.mock.calls.map((c) => c[0])).toEqual(emitted);
  });

  // --- the self-update ABI selection ------------------------------------
  //
  // Before per-ABI splits this code preferred `android-arm64` and it was
  // correct only by accident, because the release script publishes a single
  // `android-universal` key for a fat APK so the arm64 arm never matched. The
  // moment splits ship, every armv7 and x86_64 device downloads an arm64 APK
  // whose native library its CPU cannot load, AFTER the signature and hash
  // checks pass. Nothing else in this file would notice.

  /** A signed manifest carrying the given ABI keys. `verifyEd25519Detached` is
   *  mocked to accept, so these exercise the SELECTION rather than the trust
   *  path, which has its own coverage. */
  let fetched: string[] = [];
  function manifestWith(abis: Record<string, { url: string; sha256: string }>) {
    fetched = [];
    const body = JSON.stringify({ version: "9.9.9", notes: "n", abis });
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) => {
        fetched.push(url);
        if (url.endsWith(".sig")) return { ok: true, text: async () => "sig" };
        if (url.endsWith(".apk")) {
          let done = false;
          return {
            ok: true,
            headers: { get: () => "4" },
            body: {
              getReader: () => ({
                read: async () =>
                  done
                    ? { done: true, value: undefined }
                    : ((done = true), { done: false, value: new Uint8Array([1, 2, 3, 4]) }),
              }),
            },
          };
        }
        return { ok: true, arrayBuffer: async () => new TextEncoder().encode(body).buffer };
      }),
    );
  }
  const fetchSpyUrls = () => fetched;

  const APK = (n: string) => ({ url: `https://x/${n}.apk`, sha256: "a".repeat(64) });

  it("picks the APK for THIS device's ABI, not the first one published", async () => {
    invoke.mockImplementation(async (cmd: string) =>
      cmd === "client_abi" ? "android-x86_64" : cmd === "client_channel" ? "dev" : undefined,
    );
    manifestWith({
      "android-arm64": APK("arm64"),
      "android-x86_64": APK("x86_64"),
      "android-armv7": APK("armv7"),
    });
    const handle = await platform().updater.check();
    expect(handle?.version).toBe("9.9.9");
    // The assertion that matters, and it needs the DOWNLOAD rather than the
    // check: `check()` only resolves the entry, so asserting on it alone would
    // pass against a resolver that picked arm64. arm64 is FIRST in this
    // manifest and is exactly what the old code took.
    await handle?.downloadAndInstall(() => {});
    expect(fetchSpyUrls()).toContain("https://x/x86_64.apk");
    expect(fetchSpyUrls()).not.toContain("https://x/arm64.apk");
  });

  it("falls back to the universal APK while fat APKs are still published", async () => {
    invoke.mockImplementation(async (cmd: string) =>
      cmd === "client_abi" ? "android-x86_64" : cmd === "client_channel" ? "dev" : undefined,
    );
    manifestWith({ "android-universal": APK("universal") });
    const handle = await platform().updater.check();
    expect(handle?.version).toBe("9.9.9");
  });

  it("REFUSES rather than guessing when this device's ABI is not published", async () => {
    // The whole point. The old final fallback took `Object.values(abis)[0]`,
    // which installs an unloadable APK. Declining leaves the user on a working
    // build, which is the strictly better failure.
    invoke.mockImplementation(async (cmd: string) =>
      cmd === "client_abi" ? "android-x86_64" : cmd === "client_channel" ? "dev" : undefined,
    );
    manifestWith({ "android-arm64": APK("arm64"), "android-armv7": APK("armv7") });
    await expect(platform().updater.check()).resolves.toBeNull();
  });

  it("refuses when the shell is too old to report an ABI and only splits are published", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "client_abi") throw new Error("no such command");
      return cmd === "client_channel" ? "dev" : undefined;
    });
    manifestWith({ "android-arm64": APK("arm64") });
    await expect(platform().updater.check()).resolves.toBeNull();
  });

  it("LAN discovery stays empty: a phone sweeps nothing", async () => {
    await expect(platform().net.discoverCores()).resolves.toEqual([]);
  });
});
