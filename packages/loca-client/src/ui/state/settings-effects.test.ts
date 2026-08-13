// settings-effects: the side-effect listeners must react to settings
// transitions from EVERY origin. The load-origin case is the bug that
// motivated the settings rework: a core baseline merged after pairing set
// tts.enabled=true but never armed the TTS player.

import { beforeEach, describe, expect, it, vi } from "vitest";

const fake = vi.hoisted(() => ({
  entry: null as { id: string } | null,
  coreFile: {} as Record<string, unknown>,
  connListeners: new Set<(s: string) => void>(),
}));

vi.mock("$lib/core", () => ({
  cores: () => ({
    currentEntry: () => fake.entry,
    api: () => ({
      settings: {
        async load() {
          return JSON.parse(JSON.stringify(fake.coreFile));
        },
        async patch(p: Record<string, unknown>) {
          return p;
        },
        async listSecrets() {
          return [];
        },
        async setSecret() {},
        async deleteSecret() {},
      },
    }),
    subscribe() {
      return () => {};
    },
    subscribeWs() {
      return () => {};
    },
    subscribeConnectionState(fn: (s: string) => void) {
      fake.connListeners.add(fn);
      return () => fake.connListeners.delete(fn);
    },
  }),
}));

vi.mock("./tts.svelte", () => ({
  ttsState: { setEnabled: vi.fn() },
}));

vi.mock("./vad.svelte", () => ({
  vadManager: { forceDisable: vi.fn() },
}));

import { type Platform, setPlatform } from "$lib/platform";
import { settingsState } from "./settings.svelte";
import { ttsState } from "./tts.svelte";
import { vadManager } from "./vad.svelte";
import "./settings-effects";

setPlatform({
  clientFiles: {
    async read() {
      return {};
    },
    async write() {},
  },
  shortcuts: {
    async setBinding() {},
    async validate() {},
  },
} as unknown as Platform);
settingsState.attach();

const settle = (ms = 280) => new Promise((r) => setTimeout(r, ms));

beforeEach(async () => {
  vi.clearAllMocks();
  fake.entry = null;
  fake.coreFile = {};
  await settingsState
    .updateSettings({
      "tts.enabled": false,
      "stt.autoSend": false,
      "stt.llmChainTranscription": false,
    })
    .catch(() => {});
  await settle();
  vi.clearAllMocks();
});

describe("settings-effects", () => {
  it("arms TTS when tts.enabled arrives via a core baseline load", async () => {
    fake.entry = { id: "core-a" };
    fake.coreFile = { "tts.enabled": true };
    for (const fn of fake.connListeners) fn("connected");
    await vi.waitFor(() => expect(ttsState.setEnabled).toHaveBeenCalledWith(true));
  });

  it("disarms TTS and VAD when the toggles flip off", async () => {
    await settingsState.updateSetting("tts.enabled", true);
    expect(ttsState.setEnabled).toHaveBeenLastCalledWith(true);
    await settingsState.updateSetting("tts.enabled", false);
    expect(ttsState.setEnabled).toHaveBeenLastCalledWith(false);

    // STT is off by default, so arm it first to exercise the disarm transition.
    await settingsState.updateSetting("stt.enabled", true);
    await settingsState.updateSetting("stt.enabled", false);
    expect(vadManager.forceDisable).toHaveBeenCalled();
  });

  it("keeps autoSend and llmChainTranscription mutually exclusive and converges", async () => {
    await settingsState.updateSetting("stt.autoSend", true);
    await settingsState.updateSetting("stt.llmChainTranscription", true);
    // The writeback listener flips autoSend off; give its own flush a beat.
    await settle();
    expect(settingsState.currentSettings["stt.autoSend"]).toBe(false);
    expect(settingsState.currentSettings["stt.llmChainTranscription"]).toBe(true);
  });
});
