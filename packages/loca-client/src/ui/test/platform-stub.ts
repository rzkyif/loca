// A no-op stub of the Platform interface for unit tests. The app installs a
// real platform at boot (`installTauriPlatform`); under vitest there is no
// Tauri host, so tests that touch `platform()` install this stub instead.
//
// Behavior contract: host-only surfaces either no-op or throw (a test that
// needs one should assert against that), while the client-settings/keychain/
// snippet stores are backed by jsdom `localStorage` so settings flushes have
// somewhere to land. `net` rejects: unit tests never pair a core.

import { type Platform, setPlatform } from "../lib/platform/index.ts";

/** Install the stub as the active platform for the current test file. */
export function installPlatformStub(): void {
  setPlatform(impl);
}

const NOOP = async (): Promise<void> => {
  /* noop */
};
const STORAGE_PREFIX = "loca:keychain:";
const CLIENT_FILE_PREFIX = "loca:client:";
const SNIPPET_PREFIX = "loca:snippet:";
const unavailable = (what: string) => () =>
  Promise.reject(new Error(`${what} not available in the test stub`));

const impl: Platform = {
  net: {
    fetch: unavailable("net.fetch"),
    connectWebSocket: unavailable("net.connectWebSocket"),
    discoverCores: async () => [],
  },
  windowing: {
    show: NOOP,
    hide: NOOP,
    toggle: NOOP,
    requestHide: NOOP,
    position: () => Promise.resolve(),
    isVisible: async () => true,
    subscribeVisibility: async () => () => {},
    outerSize: async () => ({
      width: window.innerWidth,
      height: window.innerHeight,
    }),
    outerPosition: async () => ({ x: window.screenX, y: window.screenY }),
    setOuterSize: unavailable("windowing.setOuterSize"),
    setOuterPosition: unavailable("windowing.setOuterPosition"),
    currentMonitor: async () => null,
    subscribeHideRequested: async () => () => {},
    subscribeMonitorChanged: async () => () => {},
  },
  backButton: {
    subscribe: async () => () => {},
    exit: () => Promise.resolve(),
    canExit: () => true,
  },
  autostart: {
    isEnabled: async () => false,
    setEnabled: () => Promise.resolve(),
    wasAutostarted: async () => false,
  },
  capture: {
    monitors: async () => [],
    captureMonitor: unavailable("capture.captureMonitor"),
    setRegionTarget: NOOP,
    getRegionTarget: async () => "primary",
    showRegionOverlay: async () => "primary",
    hideRegionOverlay: NOOP,
    subscribeRegionResult: async () => () => {},
  },
  audio: {
    getSystemVolume: async () => 100,
    setSystemVolume: NOOP,
    restoreSystemVolume: NOOP,
  },
  fonts: {
    list: async () => [],
  },
  process: {
    selfMetrics: async () => ({ pid: 0, rssMb: 0, cpuPct: 0 }),
  },
  clientStorage: {
    tree: async () => ({ categories: [], total_size: 0, root_path: "" }),
    truncateActiveLog: async () => {},
  },
  shortcuts: {
    setBinding: NOOP,
    validate: async () => {},
    setPttConfig: NOOP,
    subscribeEvents: async () => () => {},
    setInputBindings: NOOP,
    subscribeInputEvents: async () => () => {},
  },
  clientFiles: {
    async read(file) {
      try {
        const raw = localStorage.getItem(CLIENT_FILE_PREFIX + file);
        return raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
      } catch {
        return {};
      }
    },
    async write(file, data) {
      localStorage.setItem(CLIENT_FILE_PREFIX + file, JSON.stringify(data));
    },
  },
  snippetFiles: {
    async readAll() {
      const out: Record<string, Record<string, unknown>> = {};
      for (let i = 0; i < localStorage.length; i++) {
        const key = localStorage.key(i);
        if (!key || !key.startsWith(SNIPPET_PREFIX)) continue;
        try {
          const raw = localStorage.getItem(key);
          if (raw) out[key.slice(SNIPPET_PREFIX.length)] = JSON.parse(raw);
        } catch {
          // Unparseable entries are skipped.
        }
      }
      return out;
    },
    async write(name, data) {
      localStorage.setItem(SNIPPET_PREFIX + name, JSON.stringify(data));
    },
    async delete(name) {
      localStorage.removeItem(SNIPPET_PREFIX + name);
    },
  },
  keychain: {
    async set(coreId, token) {
      localStorage.setItem(STORAGE_PREFIX + coreId, token);
    },
    async get(coreId) {
      return localStorage.getItem(STORAGE_PREFIX + coreId);
    },
    async delete(coreId) {
      localStorage.removeItem(STORAGE_PREFIX + coreId);
    },
  },
  pairing: {
    async readAdminToken() {
      return null;
    },
    async readLocalCoreBootError() {
      return null;
    },
    isLocalCoreInstalled() {
      return Promise.resolve(false);
    },
    isLocalCoreRunning() {
      return Promise.resolve(false);
    },
    startLocalCore() {
      return Promise.resolve(false);
    },
    stopLocalCore() {
      return Promise.resolve();
    },
    localCoreBaseUrl() {
      return Promise.resolve("https://127.0.0.1:7800");
    },
    localSidecarPorts() {
      return Promise.resolve({ llm: 7701, stt: 7702 });
    },
    launchPrefill() {
      return Promise.resolve(null);
    },
  },
  fileConvert: {
    toMarkdown: unavailable("fileConvert.toMarkdown"),
    toMarkdownFromPath: unavailable("fileConvert.toMarkdownFromPath"),
  },
  resolvePath: async (path) => path,
  openExternal: async () => {},
  revealPath: async () => {},
  updater: {
    async getVersion() {
      return "0.0.0-stub";
    },
    async check() {
      return null;
    },
    async canSelfInstall() {
      return true;
    },
    relaunch: unavailable("updater.relaunch"),
  },
  fs: {
    readFile: unavailable("fs.readFile"),
    writeFile: unavailable("fs.writeFile"),
    remove: unavailable("fs.remove"),
    tempDir: unavailable("fs.tempDir"),
    // join() is a pure-string utility, so the stub implements it for real.
    async join(...segments) {
      return segments
        .filter((s) => s.length > 0)
        .map((s, i) => (i === 0 ? s.replace(/\/+$/, "") : s.replace(/^\/+|\/+$/g, "")))
        .join("/");
    },
  },
  dialog: {
    openFilePicker: unavailable("dialog.openFilePicker"),
  },
  cursor: {
    getPosition: unavailable("cursor.getPosition"),
    setClickthrough: NOOP,
  },
  menu: {
    showContextMenu: unavailable("menu.showContextMenu"),
  },
  tray: {
    setCoreState: NOOP,
    subscribeCoreAction: async () => () => {},
  },
  monitors: {
    primary: async () => null,
    available: async () => [],
  },
  logging: {
    log(level, scope, message) {
      (console[level] ?? console.log)(scope ? `[${scope}] ${message}` : message);
    },
  },
};
