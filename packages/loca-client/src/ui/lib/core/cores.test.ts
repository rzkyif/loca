// CoresRegistry: cores.json CRUD against an in-memory platform mock (registry
// file + keychain). select()/connection behavior needs a live core and is
// exercised by the e2e flows, not here.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { platform, type Platform, setPlatform } from "$lib/platform";
import { fakeLocalCore, type FakeLocalCore } from "../../test/local-core-fake";
import { installPlatformStub } from "../../test/platform-stub";
import {
  cores,
  dropTokenlessCores,
  dropUnauthorizedCore,
  ensureLocalCoreUpIfNeeded,
  isLocalCoreEntry,
  isLoopbackUrl,
  isThisDevicesCore,
  isThisDevicesCoreStopped,
  LOCAL_CORE_ID,
  type PairedCoreEntry,
  pruneUninstalledLocalCores,
} from "./cores";

// Mutable knob for platform().pairing.isLocalCoreInstalled: `true`/`false`
// return that, `"throw"` rejects (the best-effort error path).
type LocalCoreInstalled = boolean | "throw";

function makeMockPlatform(
  localCoreInstalled: LocalCoreInstalled = true,
  localCore: FakeLocalCore | null = null,
): {
  platform: Platform;
  files: Record<string, Record<string, unknown>>;
  tokens: Record<string, string>;
  keychainGets: string[];
} {
  const files: Record<string, Record<string, unknown>> = {};
  const tokens: Record<string, string> = {};
  const keychainGets: string[] = [];
  const platform = {
    localCore,
    clientFiles: {
      async read(file: string) {
        return JSON.parse(JSON.stringify(files[file] ?? {}));
      },
      async write(file: string, data: Record<string, unknown>) {
        files[file] = JSON.parse(JSON.stringify(data));
      },
    },
    keychain: {
      async set(coreId: string, token: string) {
        tokens[coreId] = token;
      },
      async get(coreId: string) {
        keychainGets.push(coreId);
        return tokens[coreId] ?? null;
      },
      async delete(coreId: string) {
        delete tokens[coreId];
      },
    },
    pairing: {
      async isLocalCoreInstalled() {
        if (localCoreInstalled === "throw") throw new Error("check failed");
        return localCoreInstalled;
      },
      startLocalCore: vi.fn(async () => true),
      localCoreBaseUrl: vi.fn(async () => (localCore ? "" : "https://127.0.0.1:7800")),
      isLocalCoreRunning: vi.fn(async () => false),
    },
  } as unknown as Platform;
  return { platform, files, tokens, keychainGets };
}

const entry = (id: string, name = "Local Core"): PairedCoreEntry => ({
  id,
  name,
  baseUrl: "https://127.0.0.1:7800",
  trustMode: "pin",
  tlsPin: "pin",
  addedAtMs: 1,
});

const remoteEntry = (id: string, name = "Remote Core"): PairedCoreEntry => ({
  id,
  name,
  baseUrl: "https://192.168.1.5:7800",
  trustMode: "pin",
  tlsPin: "pin",
  addedAtMs: 1,
});

describe("isLoopbackUrl", () => {
  it("matches loopback hosts", () => {
    expect(isLoopbackUrl("https://127.0.0.1:7800")).toBe(true);
    expect(isLoopbackUrl("https://localhost:7800")).toBe(true);
    expect(isLoopbackUrl("https://127.0.0.2:7800")).toBe(true);
    expect(isLoopbackUrl("https://[::1]:7800")).toBe(true);
  });

  it("does NOT match remote hosts that merely contain a loopback substring", () => {
    // The old substring check would misread these as local and delete them.
    expect(isLoopbackUrl("https://localhost.example.com:7800")).toBe(false);
    expect(isLoopbackUrl("https://127.0.0.1.evil.com:7800")).toBe(false);
    expect(isLoopbackUrl("https://mylocalhost.net")).toBe(false);
    expect(isLoopbackUrl("https://192.168.1.5:7800")).toBe(false);
  });

  it("treats an unparseable URL as remote (never pruned)", () => {
    expect(isLoopbackUrl("not a url")).toBe(false);
    expect(isLoopbackUrl("")).toBe(false);
  });
});

describe("isThisDevicesCore", () => {
  // The admin-token gate in Settings > Cores: only THE platform-resolved local
  // core may ever be handed the on-disk admin token.
  const LOCAL = "https://127.0.0.1:7810";

  it("matches only the exact local origin", () => {
    expect(isThisDevicesCore("https://127.0.0.1:7810", LOCAL)).toBe(true);
    expect(isThisDevicesCore("https://127.0.0.1:7810/", LOCAL)).toBe(true);
  });

  it("rejects a loopback URL that is NOT the local core (an SSH tunnel)", () => {
    // A remote core tunneled through loopback pairs on a different port; it
    // must never receive this device's admin credential.
    expect(isThisDevicesCore("https://127.0.0.1:9922", LOCAL)).toBe(false);
    expect(isThisDevicesCore("https://localhost:7810", LOCAL)).toBe(false);
    expect(isThisDevicesCore("https://127.0.0.1:7800", LOCAL)).toBe(false);
  });

  it("rejects remote hosts, unresolved local URL, and garbage", () => {
    expect(isThisDevicesCore("https://192.168.1.5:7810", LOCAL)).toBe(false);
    expect(isThisDevicesCore("https://127.0.0.1:7810", null)).toBe(false);
    expect(isThisDevicesCore("not a url", LOCAL)).toBe(false);
  });
});

describe("isThisDevicesCoreStopped", () => {
  // The CoreClient's local-stopped probe: only THIS device's core, and only when
  // its process is down, earns the Stopped pill (which offers to start it).
  const LOCAL = "https://127.0.0.1:7800";

  function platformWith(running: boolean): Platform {
    return {
      pairing: {
        localCoreBaseUrl: async () => LOCAL,
        isLocalCoreRunning: async () => running,
      },
    } as unknown as Platform;
  }

  it("is true only for this device's core when its process is down", async () => {
    setPlatform(platformWith(false));
    expect(await isThisDevicesCoreStopped(LOCAL)).toBe(true);
  });

  it("is false when this device's core is running", async () => {
    setPlatform(platformWith(true));
    expect(await isThisDevicesCoreStopped(LOCAL)).toBe(false);
  });

  it("is false for a remote core tunneled through loopback, even with nothing on the local port", async () => {
    // Same loopback host, different port: not our core, so we must not offer to
    // start it, and the ordinary reconnect path has to keep running.
    setPlatform(platformWith(false));
    expect(await isThisDevicesCoreStopped("https://127.0.0.1:9922")).toBe(false);
    expect(await isThisDevicesCoreStopped("https://192.168.1.5:7800")).toBe(false);
  });
});

describe("cores registry", () => {
  it("addPaired writes cores.json and the keychain; list reads it back", async () => {
    const { platform, files, tokens } = makeMockPlatform();
    setPlatform(platform);

    await cores().addPaired(entry("core-a"), "token-a");
    expect(tokens["core-a"]).toBe("token-a");
    expect(files.cores).toEqual({
      cores: [entry("core-a")],
      currentCoreId: "core-a",
    });
    expect(await cores().list()).toEqual([entry("core-a")]);
  });

  it("addPaired rejects (and records nothing) when the keychain write doesn't persist", async () => {
    const { platform, files } = makeMockPlatform();
    // Simulate a silently-failing keychain: set() resolves but stores nothing.
    platform.keychain.set = async () => {};
    setPlatform(platform);

    await expect(cores().addPaired(entry("core-a"), "token-a")).rejects.toThrow(/keychain/i);
    // The core must NOT be recorded: a registry entry with no readable token is
    // exactly the dead-connection state this guards against.
    expect(files.cores).toBeUndefined();
  });

  it("removePaired drops the entry, its token, and re-points current", async () => {
    const { platform, files, tokens } = makeMockPlatform();
    setPlatform(platform);
    await cores().addPaired(entry("core-a"), "token-a");
    await cores().addPaired(entry("core-b"), "token-b");

    await cores().removePaired("core-b");
    expect(tokens["core-b"]).toBeUndefined();
    expect(files.cores).toEqual({
      cores: [entry("core-a")],
      currentCoreId: "core-a",
    });

    // Removing the last core leaves an empty registry with no current pointer.
    await cores().removePaired("core-a");
    expect(files.cores).toEqual({ cores: [] });
  });

  it("pruneUninstalledLocalCores drops local cores when no core is installed on disk", async () => {
    const { platform, files, tokens } = makeMockPlatform(false);
    setPlatform(platform);
    await cores().addPaired(entry("local-a"), "token-a");

    await pruneUninstalledLocalCores();
    // The local core's record and token are gone: a reinstall over it starts clean.
    expect(tokens["local-a"]).toBeUndefined();
    expect(files.cores).toEqual({ cores: [] });
  });

  it("pruneUninstalledLocalCores keeps local cores when a core is installed on disk", async () => {
    const { platform, files } = makeMockPlatform(true);
    setPlatform(platform);
    await cores().addPaired(entry("local-a"), "token-a");

    await pruneUninstalledLocalCores();
    expect(files.cores).toEqual({ cores: [entry("local-a")], currentCoreId: "local-a" });
  });

  it("pruneUninstalledLocalCores never touches remote cores", async () => {
    // No local core on disk, but the paired core is remote (LAN host), so it must
    // survive: only loopback cores depend on a local install.
    const { platform, files } = makeMockPlatform(false);
    setPlatform(platform);
    await cores().addPaired(remoteEntry("remote-a"), "token-a");

    await pruneUninstalledLocalCores();
    expect(files.cores).toEqual({
      cores: [remoteEntry("remote-a")],
      currentCoreId: "remote-a",
    });
  });

  it("pruneUninstalledLocalCores does not drop a remote core whose host contains 'localhost'", async () => {
    // Regression guard for the substring-vs-host bug: a remote core at
    // localhost.example.com is NOT local and must survive even with no local core.
    const { platform, files } = makeMockPlatform(false);
    setPlatform(platform);
    const trap = { ...remoteEntry("remote-a"), baseUrl: "https://localhost.example.com:7800" };
    await cores().addPaired(trap, "token-a");

    await pruneUninstalledLocalCores();
    expect(files.cores).toEqual({ cores: [trap], currentCoreId: "remote-a" });
  });

  it("pruneUninstalledLocalCores leaves cores paired when the install check throws", async () => {
    // Best-effort: an errored on-disk check must not unpair a possibly-valid core.
    const { platform, files } = makeMockPlatform("throw");
    setPlatform(platform);
    await cores().addPaired(entry("local-a"), "token-a");

    await pruneUninstalledLocalCores();
    expect(files.cores).toEqual({ cores: [entry("local-a")], currentCoreId: "local-a" });
  });

  it("pruneUninstalledLocalCores never touches the Core inside this app", async () => {
    // Its base URL is a sentinel with no host, so it is not loopback and the
    // "is a core binary on disk" question does not apply to it at all.
    const { platform, files } = makeMockPlatform(false, fakeLocalCore());
    setPlatform(platform);
    await cores().adoptLocalCore("This Device");

    await pruneUninstalledLocalCores();
    expect((files.cores.cores as PairedCoreEntry[])[0].id).toBe(LOCAL_CORE_ID);
  });

  it("rename updates the entry in place and keeps the current pointer", async () => {
    const { platform, files } = makeMockPlatform();
    setPlatform(platform);
    await cores().addPaired(entry("core-a"), "token-a");

    await cores().rename("core-a", "Desk Core");
    expect(files.cores).toEqual({
      cores: [entry("core-a", "Desk Core")],
      currentCoreId: "core-a",
    });
  });
});

// The Core inside this app: a reserved registry entry that carries no token, no
// pin and no origin, and is selected through the in-process transport.
describe("the reserved local-core entry", () => {
  const LOCAL_URL = "https://local.loca.invalid";

  beforeEach(() => {
    // The registry is a module singleton, so a `select()` in one test would
    // otherwise leak its current core into the next.
    cores().dispose();
  });

  it("adoptLocalCore records it without touching pairing or the keychain", async () => {
    const { platform, files, tokens, keychainGets } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);

    await cores().adoptLocalCore("This Device");

    expect(files.cores).toEqual({
      cores: [
        {
          id: LOCAL_CORE_ID,
          name: "This Device",
          baseUrl: LOCAL_URL,
          trustMode: "webpki",
          addedAtMs: expect.any(Number),
        },
      ],
      currentCoreId: LOCAL_CORE_ID,
    });
    expect(tokens).toEqual({});
    expect(keychainGets).toEqual([]);
  });

  it("adoptLocalCore refuses on a build with no Core inside it", async () => {
    const { platform, files } = makeMockPlatform(true, null);
    setPlatform(platform);
    await expect(cores().adoptLocalCore("This Device")).rejects.toThrow(/no Core inside/);
    expect(files.cores).toBeUndefined();
  });

  it("adoptLocalCore is idempotent: adopting twice renames rather than duplicates", async () => {
    // The existing timestamp is seeded rather than read back from a first
    // adopt: two adopts in the same millisecond produce the same `Date.now()`,
    // so comparing them would pass whether or not the original was preserved.
    const { platform, files } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);
    await platform.clientFiles.write("cores", {
      cores: [
        {
          id: LOCAL_CORE_ID,
          name: "This Device",
          baseUrl: LOCAL_URL,
          trustMode: "webpki",
          addedAtMs: 1234,
        },
      ],
      currentCoreId: LOCAL_CORE_ID,
    });

    await cores().adoptLocalCore("My Phone");
    const list = files.cores.cores as PairedCoreEntry[];
    expect(list).toHaveLength(1);
    expect(list[0].name).toBe("My Phone");
    expect(list[0].addedAtMs).toBe(1234);
  });

  it("addPaired refuses the reserved id, which has no token to store", async () => {
    const { platform, files } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);
    await expect(
      cores().addPaired({ ...entry(LOCAL_CORE_ID), baseUrl: LOCAL_URL }, "tok"),
    ).rejects.toThrow(/reserved/i);
    expect(files.cores).toBeUndefined();
  });

  it("rebuilds its canonical fields on read, so cores.json cannot redirect it", async () => {
    // The reserved id skips the keychain and the pin, so a hand-edited (or
    // restored-from-elsewhere) file pointing it at a network host would inherit
    // that path against a real server. Only the name and the timestamp are the
    // file's to state.
    const { platform } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);
    await platform.clientFiles.write("cores", {
      cores: [
        {
          id: LOCAL_CORE_ID,
          name: "This Device",
          baseUrl: "https://evil.example.com:7800",
          trustMode: "pin",
          tlsPin: "ATTACKER",
          addedAtMs: 7,
        },
      ],
      currentCoreId: LOCAL_CORE_ID,
    });

    expect(await cores().list()).toEqual([
      {
        id: LOCAL_CORE_ID,
        name: "This Device",
        baseUrl: LOCAL_URL,
        trustMode: "webpki",
        addedAtMs: 7,
      },
    ]);
  });

  // A cores.json can arrive on a build that embeds no Core: a restored backup, a
  // synced home directory, a hand edit. Every canonical field is rebuilt on
  // read, but "does this build embed a Core at all" is the one fact that decides
  // whether the entry can do anything, and it is not on disk to rebuild from.
  describe("on a build with no Core inside it", () => {
    const staleFile = {
      cores: [
        {
          id: LOCAL_CORE_ID,
          name: "This Device",
          baseUrl: LOCAL_URL,
          trustMode: "webpki",
          addedAtMs: 1,
        },
        remoteEntry("remote-a"),
      ],
      currentCoreId: LOCAL_CORE_ID,
    };

    it("is dropped on the read, which is where every recovery path looks", async () => {
      // Dropping it in `select` alone would leave it listed in Settings, left on
      // disk, and skipped by all three sweeps that exist to clear a persisted
      // core that cannot work.
      const { platform } = makeMockPlatform(true, null);
      setPlatform(platform);
      await platform.clientFiles.write("cores", staleFile);

      expect((await cores().list()).map((c) => c.id)).toEqual(["remote-a"]);
      await expect(cores().select(LOCAL_CORE_ID)).rejects.toThrow(/not in paired list/);
    });

    it("does not strand a working sibling behind it", async () => {
      // A resolved `select` is success to restoreSelected, so an entry that
      // selects cleanly and can serve nothing stops the search on the one core
      // that would have worked.
      const { platform, tokens } = makeMockPlatform(true, null);
      setPlatform(platform);
      tokens["remote-a"] = "token-a";
      await platform.clientFiles.write("cores", staleFile);

      await cores().restoreSelected();
      expect(cores().currentEntry()?.id).toBe("remote-a");
    });
  });

  it("select reads no token and hands the client the in-process transport", async () => {
    // The transport half is the mount assertion: a perfect local transport that
    // the registry never selects is worth nothing.
    const local = fakeLocalCore();
    const { platform, keychainGets } = makeMockPlatform(true, local);
    setPlatform(platform);
    await cores().adoptLocalCore("This Device");

    await cores().select(LOCAL_CORE_ID);

    expect(keychainGets).toEqual([]);
    expect(cores().currentEntry()?.id).toBe(LOCAL_CORE_ID);
    // Drive one request through the built client and watch it land in process
    // rather than on the network stub (which would throw).
    local.reply({ status: 200, bodyB64: btoa('{"status":"ok"}') });
    await cores().currentClient()?.get("/api/v1/health");
    expect(local.requests.at(-1)?.url).toBe("/api/v1/health");
  });

  it("select still reads the keychain for every other core", async () => {
    const { platform, keychainGets } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);
    await cores().addPaired(remoteEntry("remote-a"), "token-a");

    await cores().select("remote-a");
    expect(keychainGets).toContain("remote-a");
  });

  it("select still refuses an ordinary core whose token is gone", async () => {
    // The token skip is for the reserved id ONLY. An ordinary core with no
    // token must keep failing loudly here: a client built with an empty bearer
    // would connect, be rejected, and present as a mysterious auth failure
    // rather than as the missing credential it is.
    const { platform, tokens } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);
    await cores().addPaired(remoteEntry("remote-a"), "token-a");
    delete tokens["remote-a"];

    await expect(cores().select("remote-a")).rejects.toThrow(/no token/);
  });

  it("removePaired drops it without a keychain delete", async () => {
    const local = fakeLocalCore();
    const { platform, files } = makeMockPlatform(true, local);
    let deletes = 0;
    platform.keychain.delete = async () => {
      deletes++;
    };
    setPlatform(platform);
    await cores().adoptLocalCore("This Device");

    await cores().removePaired(LOCAL_CORE_ID);
    expect(deletes).toBe(0);
    expect(files.cores).toEqual({ cores: [] });
  });

  it("ensureLocalCoreUpIfNeeded starts it, despite its URL not being loopback", async () => {
    // The reserved entry has to be recognized explicitly: a loopback test reads
    // its sentinel URL as remote and would skip the one core it is most sure
    // about, deferring a multi-second boot onto the first request instead.
    const { platform } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);
    await cores().adoptLocalCore("This Device");
    await cores().select(LOCAL_CORE_ID);

    await ensureLocalCoreUpIfNeeded();
    expect(platform.pairing.startLocalCore).toHaveBeenCalled();
  });

  it("ensureLocalCoreUpIfNeeded leaves a remote core alone", async () => {
    const { platform } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);
    await cores().addPaired(remoteEntry("remote-a"), "token-a");
    await cores().select("remote-a");

    await ensureLocalCoreUpIfNeeded();
    expect(platform.pairing.startLocalCore).not.toHaveBeenCalled();
  });

  it("the auth-rejection unpair refuses it, though its bearer cannot be rejected", async () => {
    // A pre-authenticated in-process client cannot have its bearer rejected, so
    // reaching here at all would be a bug somewhere else. Acting on it anyway
    // would silently unpair the only Core the device has. This lives next to
    // the tokenless sweep rather than in the component that calls it, because
    // that component has no test and a guard left there is a guard nothing can
    // watch fail.
    const { platform, files } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);
    await cores().adoptLocalCore("This Device");
    await cores().select(LOCAL_CORE_ID);

    expect(await dropUnauthorizedCore()).toBeNull();
    expect((files.cores.cores as PairedCoreEntry[]).map((c) => c.id)).toEqual([LOCAL_CORE_ID]);
  });

  it("the auth-rejection unpair still drops an ordinary core whose bearer died", async () => {
    const { platform, files } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);
    await cores().addPaired(remoteEntry("remote-a"), "token-a");
    await cores().select("remote-a");

    expect(await dropUnauthorizedCore()).toBe("remote-a");
    expect((files.cores.cores as PairedCoreEntry[]).map((c) => c.id)).toEqual([]);
  });

  it("the tokenless sweep never unpairs it, though it has no token at all", async () => {
    // Its client identity is injected below the command boundary, so there is
    // nothing in the keychain to find. Without the skip, the one core a phone
    // is certain of is the one this sweep deletes.
    const { platform, files } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);
    await cores().adoptLocalCore("This Device");

    expect(await dropTokenlessCores()).toEqual([]);
    expect((files.cores.cores as PairedCoreEntry[]).map((c) => c.id)).toEqual([LOCAL_CORE_ID]);
  });

  it("the tokenless sweep still unpairs an ordinary core whose token is gone", async () => {
    const { platform, tokens, files } = makeMockPlatform(true, fakeLocalCore());
    setPlatform(platform);
    await cores().adoptLocalCore("This Device");
    await cores().addPaired(remoteEntry("remote-a"), "token-a");
    delete tokens["remote-a"];

    expect(await dropTokenlessCores()).toEqual(["remote-a"]);
    expect((files.cores.cores as PairedCoreEntry[]).map((c) => c.id)).toEqual([LOCAL_CORE_ID]);
  });

  it("the tokenless sweep leaves a core paired when the keychain read throws", async () => {
    const { platform, files } = makeMockPlatform(true, null);
    setPlatform(platform);
    await cores().addPaired(remoteEntry("remote-a"), "token-a");
    platform.keychain.get = async () => {
      throw new Error("keychain locked");
    };

    expect(await dropTokenlessCores()).toEqual([]);
    expect((files.cores.cores as PairedCoreEntry[]).map((c) => c.id)).toEqual(["remote-a"]);
  });

  it("is given no stopped-probe at all, so the probe is never even entered", async () => {
    // "Stopped" means a process this device owns is down and the user can start
    // it. The Core inside this app has no process to be down and a request boots
    // it, so the state is unreachable; offering its Start affordance would be a
    // dead end. Driven through the registry-built client so the probe wiring is
    // the shipped one.
    //
    // The assertion is on `localCoreBaseUrl`, which is the probe's FIRST call,
    // not on `isLocalCoreRunning`: the probe short-circuits before reaching the
    // latter (the reserved entry never matches the local origin), so asserting
    // on the second call would pass with the probe fully wired up.
    vi.useFakeTimers();
    try {
      const local = fakeLocalCore();
      const { platform } = makeMockPlatform(true, local);
      setPlatform(platform);
      await cores().adoptLocalCore("This Device");
      await cores().select(LOCAL_CORE_ID);

      const states: string[] = [];
      cores().subscribeConnectionState((s) => states.push(s));
      cores()
        .currentClient()
        ?.subscribe(() => {});
      await vi.advanceTimersByTimeAsync(0);
      local.emitOpen();

      // Consecutive closes with no successful open in between, so the attempt
      // counter climbs past STOPPED_PROBE_FROM_ATTEMPT instead of being reset.
      for (let i = 0; i < 4; i++) {
        local.emitClose();
        await vi.advanceTimersByTimeAsync(3000);
      }

      expect(platform.pairing.localCoreBaseUrl).not.toHaveBeenCalled();
      expect(states).not.toContain("stopped");
    } finally {
      vi.useRealTimers();
    }
  });

  it("is keyed by a literal string, because cores.json is written with it", () => {
    // Pinned as the on-disk key it is, not as a match with anything Core-side:
    // the client never sends this value across the boundary, so a rename would
    // break nothing there and would orphan every entry already adopted here.
    expect(LOCAL_CORE_ID).toBe("local");
  });

  it("isLocalCoreEntry names only the reserved id", () => {
    expect(isLocalCoreEntry({ id: LOCAL_CORE_ID })).toBe(true);
    expect(isLocalCoreEntry({ id: "local-a" })).toBe(false);
    expect(isLocalCoreEntry({ id: "remote-a" })).toBe(false);
  });

  it("is inert on a platform that carries no Core, through the SHIPPED stub", () => {
    // Every other test file in this suite runs against `installPlatformStub()`
    // and its meaning depends on that stub being desktop-shaped. Asserting it
    // through the shipped stub rather than a local mock is what makes this a
    // guard on those files rather than a restatement of this one's fixture.
    installPlatformStub();
    expect(platform().localCore).toBeNull();
    return expect(cores().adoptLocalCore("This Device")).rejects.toThrow(/no Core inside/);
  });
});
