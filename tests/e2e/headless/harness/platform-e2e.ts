// The E2E Platform implementation (runs in the browser test). Native surfaces
// are stubbed/no-op (mirroring src/ui/test/platform-stub.ts), but `net` makes
// real browser fetch / WebSocket calls to the spawned core over its self-signed
// TLS (Chromium trusts it via the context's ignoreHTTPSErrors). clientFiles and
// keychain are in-memory and seedable, so a scenario can boot the app already
// "paired" by seeding cores.json + the bearer token.

import type {
  ClientFileName,
  NetRequest,
  NetResponse,
  NetSocket,
  Platform,
} from "@client/lib/platform/index.ts";
import { setPlatform } from "@client/lib/platform/index.ts";

export interface E2ePlatformSeed {
  /** Pre-seeded client JSON files (e.g. { cores: {...}, settings: {...} }). */
  clientFiles?: Partial<Record<ClientFileName, Record<string, unknown>>>;
  /** Pre-seeded keychain bearer tokens, keyed by core id. */
  keychain?: Record<string, string>;
  /** The core's real SPKI pin (base64(SHA-256(SPKI))), computed Node-side. The
   *  pairing PAKE channel-binds this exact value, so a capture-mode fetch must
   *  report it (not a placeholder) or pairing confirmation fails. */
  tlsPin?: string;
  /** Present the spawned core as an installer-provided LOCAL core: the pairing
   *  surface reports it installed, hands back its real on-disk admin token, and
   *  points localCoreBaseUrl at it, so the app's first-boot silent pair runs
   *  against the real core (the "installer" scenario). */
  localCore?: { adminToken: string; baseUrl: string };
}

const NOOP = async (): Promise<void> => {};
const unavailable = (what: string) => () =>
  Promise.reject(new Error(`${what} not available in e2e`));

// Per-origin SPKI pins, so an app paired to more than one core hands the right
// pin back to each core's TOFU probe. Keyed by URL origin (https://127.0.0.1:port).
const pinByOrigin = new Map<string, string>();

/** Register the real SPKI pin for a core's origin (the pairing TOFU probe reads
 *  it via a capture-mode fetch). Called for every core the test pairs, including
 *  extras. */
export function registerCorePin(baseUrl: string, pin: string): void {
  pinByOrigin.set(new URL(baseUrl).origin, pin);
}

function makeRealFetch(defaultPin: string) {
  return async function realFetch(req: NetRequest): Promise<NetResponse> {
    const res = await fetch(req.url, {
      method: req.method ?? "GET",
      headers: req.headers,
      body: req.body as BodyInit | undefined,
    });
    const headers: Record<string, string> = {};
    res.headers.forEach((v, k) => (headers[k] = v));
    const body = new Uint8Array(await res.arrayBuffer());
    const out: NetResponse = { status: res.status, headers, body };
    // Pairing TOFU capture: Chromium trusts the cert via ignoreHTTPSErrors and
    // doesn't expose it, so the harness computed the real SPKI pin Node-side and
    // we hand it back here. The PAKE binds this, so it must be the true pin.
    // Resolve per-origin (multi-core) with the single-core seed as the fallback.
    if (req.mode === "capture") {
      out.capturedPin = pinByOrigin.get(new URL(req.url).origin) ?? defaultPin;
    }
    return out;
  };
}

function realSocket(url: string): Promise<NetSocket> {
  const ws = new WebSocket(url);
  return Promise.resolve({
    send: (d) => ws.send(d),
    close: () => ws.close(),
    onOpen: (cb) => {
      // The client registers onOpen after connectWebSocket() resolves; if the
      // socket already opened, fire immediately so it isn't missed (the Tauri
      // transport handles the same race).
      if (ws.readyState === WebSocket.OPEN) cb();
      else ws.addEventListener("open", () => cb());
    },
    onMessage: (cb) =>
      ws.addEventListener("message", (e) => cb(typeof e.data === "string" ? e.data : "")),
    onClose: (cb) => {
      if (ws.readyState === WebSocket.CLOSED) cb();
      else ws.addEventListener("close", () => cb());
    },
    onError: (cb) => ws.addEventListener("error", () => cb("websocket error")),
  });
}

export function buildE2ePlatform(seed: E2ePlatformSeed = {}): Platform {
  const files = new Map<string, Record<string, unknown>>();
  for (const [k, v] of Object.entries(seed.clientFiles ?? {}))
    files.set(k, v as Record<string, unknown>);
  const keys = new Map<string, string>(Object.entries(seed.keychain ?? {}));
  const snippets = new Map<string, Record<string, unknown>>();
  const realFetch = makeRealFetch(seed.tlsPin ?? "e2e-pin");

  return {
    net: { fetch: realFetch, connectWebSocket: (url) => realSocket(url) },
    windowing: {
      show: NOOP,
      hide: NOOP,
      toggle: NOOP,
      requestHide: NOOP,
      position: () => Promise.resolve(),
      isVisible: async () => true,
      subscribeVisibility: async () => () => {},
      outerSize: async () => ({ width: window.innerWidth, height: window.innerHeight }),
      outerPosition: async () => ({ x: 0, y: 0 }),
      setOuterSize: NOOP,
      setOuterPosition: NOOP,
      currentMonitor: async () => null,
      subscribeHideRequested: async () => () => {},
      subscribeMonitorChanged: async () => () => {},
    },
    // Android system back. Inert on this desktop-shaped platform (never fires),
    // matching the test platform-stub fixture.
    backButton: {
      subscribe: async () => () => {},
      exit: NOOP,
    },
    autostart: {
      isEnabled: async () => false,
      setEnabled: NOOP,
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
    fonts: { list: async () => [] },
    process: { selfMetrics: async () => ({ pid: 0, rssMb: 0, cpuPct: 0 }) },
    clientStorage: {
      tree: async () => ({ categories: [], total_size: 0, root_path: "" }),
      truncateActiveLog: NOOP,
    },
    shortcuts: {
      setBinding: NOOP,
      validate: NOOP,
      subscribeEvents: async () => () => {},
      setInputBindings: NOOP,
      subscribeInputEvents: async () => () => {},
    },
    clientFiles: {
      async read(file) {
        return files.get(file) ?? {};
      },
      async write(file, data) {
        files.set(file, data);
      },
    },
    snippetFiles: {
      async readAll() {
        return Object.fromEntries(snippets);
      },
      async write(name, data) {
        snippets.set(name, data);
      },
      async delete(name) {
        snippets.delete(name);
      },
    },
    keychain: {
      async set(coreId, token) {
        keys.set(coreId, token);
      },
      async get(coreId) {
        return keys.get(coreId) ?? null;
      },
      async delete(coreId) {
        keys.delete(coreId);
      },
    },
    pairing: {
      async readAdminToken() {
        return seed.localCore?.adminToken ?? null;
      },
      async readLocalCoreBootError() {
        return null;
      },
      isLocalCoreInstalled: async () => !!seed.localCore,
      startLocalCore: async () => false,
      localCoreBaseUrl: async () => seed.localCore?.baseUrl ?? "https://127.0.0.1:7800",
      localSidecarPorts: async () => ({ llm: 7701, stt: 7702 }),
      launchPrefill: async () => null,
    },
    fileConvert: {
      toMarkdown: unavailable("fileConvert.toMarkdown"),
      toMarkdownFromPath: unavailable("fileConvert.toMarkdownFromPath"),
    },
    resolvePath: async (p) => p,
    openExternal: NOOP,
    revealPath: NOOP,
    updater: {
      getVersion: async () => "0.0.0-e2e",
      check: async () => null,
      canSelfInstall: async () => true,
      relaunch: unavailable("updater.relaunch"),
    },
    fs: {
      readFile: unavailable("fs.readFile"),
      writeFile: unavailable("fs.writeFile"),
      remove: unavailable("fs.remove"),
      tempDir: unavailable("fs.tempDir"),
      async join(...segments) {
        return segments
          .filter((s) => s.length > 0)
          .map((s, i) => (i === 0 ? s.replace(/\/+$/, "") : s.replace(/^\/+|\/+$/g, "")))
          .join("/");
      },
    },
    dialog: { openFilePicker: unavailable("dialog.openFilePicker") },
    cursor: { getPosition: unavailable("cursor.getPosition"), setClickthrough: NOOP },
    menu: { showContextMenu: unavailable("menu.showContextMenu") },
    monitors: { primary: async () => null, available: async () => [] },
    logging: {
      log(level, scope, message) {
        const fn =
          (console as unknown as Record<string, (m: string) => void>)[level] ?? console.log;
        fn(scope ? `[${scope}] ${message}` : message);
      },
    },
  };
}

export function installE2ePlatform(seed?: E2ePlatformSeed): Platform {
  // Fresh per-origin pin registry per app install (per-file isolation also resets
  // module state, but a single file may launch several apps in sequence).
  pinByOrigin.clear();
  const p = buildE2ePlatform(seed);
  setPlatform(p);
  return p;
}
