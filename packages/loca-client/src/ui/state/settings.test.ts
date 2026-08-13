// SettingsState: the layered client/core store and its single change
// pipeline, driven through a fake cores() registry (selection, WS frames,
// connection edges) and an in-memory platform mock. Covers the regression
// that motivated the rework: core values merged outside a user edit (e.g.
// right after pairing) must fire onChange so side effects like TTS arming
// stay in sync.

import { beforeEach, describe, expect, it, vi } from "vitest";

const fake = vi.hoisted(() => ({
  entry: null as { id: string } | null,
  coreFile: {} as Record<string, unknown>,
  secretNames: [] as string[],
  patches: [] as Array<Record<string, unknown>>,
  patchError: null as Error | null,
  // When set, the baseline GET stalls until the promise resolves, so tests
  // can edit inside the selection-to-baseline window.
  loadGate: null as Promise<void> | null,
  setSecretCalls: [] as Array<[string, string]>,
  deleteSecretCalls: [] as string[],
  regListeners: new Set<() => void>(),
  wsListeners: new Set<(frame: Record<string, unknown>) => void>(),
  connListeners: new Set<(s: string) => void>(),
}));

vi.mock("$lib/core", () => ({
  cores: () => ({
    currentEntry: () => fake.entry,
    api: () => ({
      settings: {
        async load() {
          if (fake.loadGate) await fake.loadGate;
          return JSON.parse(JSON.stringify(fake.coreFile));
        },
        async patch(p: Record<string, unknown>) {
          if (fake.patchError) throw fake.patchError;
          fake.patches.push(p);
          for (const [k, v] of Object.entries(p)) {
            if (v === null || v === undefined) delete fake.coreFile[k];
            else fake.coreFile[k] = v;
          }
          return JSON.parse(JSON.stringify(fake.coreFile));
        },
        async listSecrets() {
          return [...fake.secretNames];
        },
        async setSecret(name: string, value: string) {
          fake.setSecretCalls.push([name, value]);
        },
        async deleteSecret(name: string) {
          fake.deleteSecretCalls.push(name);
        },
      },
    }),
    subscribe(fn: () => void) {
      fake.regListeners.add(fn);
      return () => fake.regListeners.delete(fn);
    },
    subscribeWs(fn: (frame: Record<string, unknown>) => void) {
      fake.wsListeners.add(fn);
      return () => fake.wsListeners.delete(fn);
    },
    subscribeConnectionState(fn: (s: string) => void) {
      fake.connListeners.add(fn);
      return () => fake.connListeners.delete(fn);
    },
  }),
}));

import { getDefaultSettings } from "@loca/shared";
import { type Platform, setPlatform } from "$lib/platform";
import { type SettingsChangeOrigin, settingsState } from "./settings.svelte";

const files: Record<string, Record<string, unknown>> = { settings: {} };

const platformMock = {
  clientFiles: {
    async read(file: string) {
      return JSON.parse(JSON.stringify(files[file] ?? {}));
    },
    async write(file: string, data: Record<string, unknown>) {
      files[file] = JSON.parse(JSON.stringify(data));
    },
  },
  shortcuts: {
    async setBinding() {},
    async validate() {},
  },
} as unknown as Platform;

const DEFAULTS = getDefaultSettings();

// Mirror the real registry: selecting (or unpairing) always notifies
// subscribers, which is what drives settingsState's core lifecycle.
function selectCore(id: string | null): void {
  fake.entry = id ? { id } : null;
  for (const fn of fake.regListeners) fn();
}
function emitWs(frame: Record<string, unknown>): void {
  for (const fn of fake.wsListeners) fn(frame);
}
function emitConnected(): void {
  for (const fn of fake.connListeners) fn("connected");
}

// Past the 200ms flush debounce plus a settling beat.
const settle = (ms = 280) => new Promise((r) => setTimeout(r, ms));

type Notification = {
  key: string;
  prev: unknown;
  next: unknown;
  origin: SettingsChangeOrigin;
};

let notifications: Notification[] = [];
let offListener: (() => void) | null = null;

function listen(): void {
  offListener = settingsState.onChange((key, prev, next, origin) => {
    notifications.push({ key, prev, next, origin });
  });
}

function notificationsFor(key: string): Notification[] {
  return notifications.filter((n) => n.key === key);
}

setPlatform(platformMock);
settingsState.attach();

beforeEach(async () => {
  offListener?.();
  offListener = null;
  notifications = [];
  // Unpair (resets the core layer through the pipeline), then clear the
  // fake's stores and reload an empty client file so every test starts from
  // pure schema defaults.
  selectCore(null);
  fake.coreFile = {};
  fake.patches = [];
  fake.patchError = null;
  fake.loadGate = null;
  fake.secretNames = [];
  fake.setSecretCalls = [];
  fake.deleteSecretCalls = [];
  files.settings = {};
  await settingsState.loadClientSettings();
  await settle();
});

describe("settingsState", () => {
  it("persists only the sparse client delta to the settings file", async () => {
    listen();
    await settingsState.updateSetting("appearance.theme", "light");
    expect(files.settings).toEqual({ "appearance.theme": "light" });
    // Reverting to the default empties the file again (sparse storage).
    await settingsState.updateSetting("appearance.theme", DEFAULTS["appearance.theme"]);
    expect(files.settings).toEqual({});
  });

  it("fires onChange for core baselines on selection and reconnect (TTS pairing regression)", async () => {
    listen();
    fake.coreFile = { "tts.enabled": true };
    selectCore("core-a");
    await vi.waitFor(() => expect(settingsState.coreLoaded).toBe(true));

    expect(settingsState.currentSettings["tts.enabled"]).toBe(true);
    expect(notificationsFor("tts.enabled")).toEqual([
      { key: "tts.enabled", prev: false, next: true, origin: "load" },
    ]);

    // A core restart changed the value while we were disconnected: the
    // reconnect edge re-baselines and fires the transition.
    fake.coreFile = {};
    emitConnected();
    await vi.waitFor(() => expect(settingsState.currentSettings["tts.enabled"]).toBe(false));
    expect(notificationsFor("tts.enabled")[1]).toMatchObject({
      prev: true,
      next: false,
      origin: "load",
    });
  });

  it("treats the WS echo of its own PATCH as a no-op", async () => {
    selectCore("core-a");
    await vi.waitFor(() => expect(settingsState.coreLoaded).toBe(true));
    listen();

    await settingsState.updateSetting("tts.enabled", true);
    expect(fake.patches).toEqual([{ "tts.enabled": true }]);
    expect(notificationsFor("tts.enabled")).toHaveLength(1);

    emitWs({
      kind: "settings.updated",
      values: { "tts.enabled": true },
      deleted: [],
    });
    await settle();
    expect(notificationsFor("tts.enabled")).toHaveLength(1);
    expect(fake.patches).toHaveLength(1);
  });

  it("applies a remote delta without PATCHing back", async () => {
    selectCore("core-a");
    await vi.waitFor(() => expect(settingsState.coreLoaded).toBe(true));
    listen();

    emitWs({
      kind: "settings.updated",
      values: { "llm.contextSize": 16384 },
      deleted: [],
    });
    expect(settingsState.currentSettings["llm.contextSize"]).toBe(16384);
    expect(notificationsFor("llm.contextSize")).toEqual([
      {
        key: "llm.contextSize",
        prev: DEFAULTS["llm.contextSize"],
        next: 16384,
        origin: "remote",
      },
    ]);
    await settle();
    expect(fake.patches).toHaveLength(0);

    emitWs({
      kind: "settings.updated",
      values: {},
      deleted: ["llm.contextSize"],
    });
    expect(settingsState.currentSettings["llm.contextSize"]).toBe(DEFAULTS["llm.contextSize"]);
  });

  it("resets core values on a core switch, then re-arms from the new baseline", async () => {
    fake.coreFile = { "tts.enabled": true };
    selectCore("core-a");
    await vi.waitFor(() => expect(settingsState.currentSettings["tts.enabled"]).toBe(true));
    listen();

    // Switch to a different core whose baseline also enables TTS: the reset
    // fires a real true -> false transition first, then the baseline re-arms.
    selectCore("core-b");
    expect(notificationsFor("tts.enabled")[0]).toMatchObject({
      prev: true,
      next: false,
    });
    await vi.waitFor(() => expect(settingsState.currentSettings["tts.enabled"]).toBe(true));
    expect(notificationsFor("tts.enabled")[1]).toMatchObject({
      prev: false,
      next: true,
      origin: "load",
    });
  });

  it("queues edits made in the selection-to-baseline window and flushes after it lands", async () => {
    listen();
    // The baseline GET is in flight (e.g. quick settings right after
    // pairing): the edit applies locally and queues instead of PATCHing
    // against an unknown baseline.
    let releaseBaseline!: () => void;
    fake.loadGate = new Promise((r) => (releaseBaseline = r));
    selectCore("core-a");
    await settingsState.updateSetting("tts.enabled", true);
    expect(settingsState.currentSettings["tts.enabled"]).toBe(true);
    expect(fake.patches).toHaveLength(0);

    // The baseline lands without the key: it must not clobber the pending
    // edit, and exactly one PATCH asserts it afterwards.
    releaseBaseline();
    fake.loadGate = null;
    await vi.waitFor(() => expect(fake.patches).toEqual([{ "tts.enabled": true }]));
    expect(settingsState.currentSettings["tts.enabled"]).toBe(true);
    expect(notificationsFor("tts.enabled")).toHaveLength(1);
  });

  it("rolls back only the failed destination's keys", async () => {
    selectCore("core-a");
    await vi.waitFor(() => expect(settingsState.coreLoaded).toBe(true));
    listen();

    fake.patchError = new Error("core unreachable");
    await expect(
      settingsState.updateSettings({
        "appearance.theme": "light",
        "llm.contextSize": 16384,
      }),
    ).rejects.toThrow("settings save failed");

    // The client file write succeeded and keeps its value; the core key
    // reverted with a reverse notification.
    expect(settingsState.currentSettings["appearance.theme"]).toBe("light");
    expect(files.settings).toEqual({ "appearance.theme": "light" });
    expect(settingsState.currentSettings["llm.contextSize"]).toBe(DEFAULTS["llm.contextSize"]);
    const llm = notificationsFor("llm.contextSize");
    expect(llm[llm.length - 1]).toMatchObject({
      next: DEFAULTS["llm.contextSize"],
    });
  });

  it("PATCHes the null reset sentinel when a core key reverts to default", async () => {
    selectCore("core-a");
    await vi.waitFor(() => expect(settingsState.coreLoaded).toBe(true));

    await settingsState.updateSetting("llm.contextSize", 16384);
    expect(fake.patches).toEqual([{ "llm.contextSize": 16384 }]);

    await settingsState.updateSetting("llm.contextSize", DEFAULTS["llm.contextSize"]);
    expect(fake.patches).toEqual([
      { "llm.contextSize": 16384 },
      {
        "llm.contextSize": null,
      },
    ]);
  });

  it("routes secrets to the vault only when dirty, never to PATCH or the file", async () => {
    selectCore("core-a");
    await vi.waitFor(() => expect(settingsState.coreLoaded).toBe(true));

    await settingsState.updateSetting("llm.external.apiKey", "sk-test");
    expect(fake.setSecretCalls).toEqual([["llm.external.apiKey", "sk-test"]]);
    expect(fake.patches).toHaveLength(0);
    expect(files.settings).toEqual({});
    expect(settingsState.isSecretConfigured("llm.external.apiKey")).toBe(true);

    // An unrelated save must not re-write the (no longer dirty) secret.
    await settingsState.updateSetting("appearance.theme", "light");
    expect(fake.setSecretCalls).toHaveLength(1);

    // settings.updated secretNames refreshes the configured set.
    emitWs({
      kind: "settings.updated",
      values: {},
      deleted: [],
      secretNames: ["dualModel.external.apiKey"],
    });
    expect(settingsState.isSecretConfigured("llm.external.apiKey")).toBe(false);
    expect(settingsState.isSecretConfigured("dualModel.external.apiKey")).toBe(true);
  });
});
