// Mobile (Tauri-mobile) implementation of the Platform interface, shared by
// Android and iOS. The few spots that differ between the two (self-update, the
// back affordance, the font picker) branch on osPlatform().
//
// The namespaces backed by cross-platform Rust commands or plugins (net, client
// files, storage, keychain, process, logging, resolvePath, fs, dialog, opener,
// app version) are identical to desktop. The desktop-only host features
// (windowing, global shortcuts, screen capture, volume ducking, autostart,
// native menus, local-core install) no-op or reject with a "not supported on
// mobile" error, and the UI hides their affordances behind
// UiContext.platform === "mobile".

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  getVersion as tauriGetVersion,
  hide as tauriHide,
  onBackButtonPress,
} from "@tauri-apps/api/app";
import { join as tauriJoin, tempDir as tauriTempDir } from "@tauri-apps/api/path";
import {
  readFile as tauriReadFile,
  remove,
  writeFile as tauriWriteFile,
} from "@tauri-apps/plugin-fs";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import { platform as osPlatform } from "@tauri-apps/plugin-os";
import { sha256Hex, timingSafeEqualHex, verifyEd25519Detached } from "@loca/shared";
import { presentActionSheet } from "$lib/menu/action-sheet-host.svelte";
import {
  type LocalCore,
  type LocalCoreInfo,
  type LocalCoreReply,
  type LocalCoreRequest,
  type Platform,
  setPlatform,
  type UpdateHandle,
} from "./index";
import { net } from "./shared";

export function installMobilePlatform(): void {
  setPlatform(impl);
}

// --- Self-hosted APK updater ----------------------------------------------
//
// Tauri's updater plugin has no Android support, so mobile self-updates read the
// same R2-hosted, Ed25519-signed manifest the release pipeline publishes. The
// manifest arrives over an unpinned connection, so before ANY field is trusted
// its detached signature is verified against the committed key, and the APK is
// checked against the signed sha256 before staging. Without both this would be
// an unauthenticated fetch-and-install path; Android's install-time keystore
// check is a backstop, not the trust gate.
//
// Mirrors packages/loca-core-rs/data/signing-keys.json. Update both on rotation.
const ANDROID_MANIFEST_PUBKEY = "KghrHOIqu76Hpl/xX8RHUuDA2n1NGCOj9gD1Jrn5H+M=";

interface AndroidManifest {
  version: string;
  notes?: string;
  abis: Record<string, { url: string; sha256: string }>;
}

/** The android.json URL for the channel baked into this build. Stable lives at
 *  /manifests; other channels nest under /manifests/<channel>/, matching the
 *  release pipeline (channelManifestDir) and the per-channel applicationId, so a
 *  non-stable install updates from its own manifest rather than stable's. */
async function androidManifestUrl(): Promise<string> {
  let channel = "stable";
  try {
    channel = (await invoke<string>("client_channel")) || "stable";
  } catch {
    // Fall back to stable if the command is unavailable.
  }
  const dir = channel === "stable" ? "manifests" : `manifests/${channel}`;
  return `https://get.loca.rzkyif.com/${dir}/android.json`;
}

/** Strictly-greater semver compare. Numeric major.minor.patch first; on a tie a
 *  release (no pre-release suffix) outranks a pre-release, and two pre-releases
 *  fall back to lexical order. Avoids `parseInt("0-rc1")` swallowing suffixes. */
function isNewerVersion(remote: string, local: string): boolean {
  const parse = (v: string): { nums: number[]; pre: string } => {
    const dash = v.indexOf("-");
    const core = dash === -1 ? v : v.slice(0, dash);
    const pre = dash === -1 ? "" : v.slice(dash + 1);
    return { nums: core.split(".").map((n) => parseInt(n, 10) || 0), pre };
  };
  const r = parse(remote);
  const l = parse(local);
  for (let i = 0; i < Math.max(r.nums.length, l.nums.length); i++) {
    const a = r.nums[i] ?? 0;
    const b = l.nums[i] ?? 0;
    if (a !== b) return a > b;
  }
  if (r.pre === l.pre) return false;
  if (!r.pre) return true; // remote is a release, local a pre-release
  if (!l.pre) return false; // remote is a pre-release, local a release
  return r.pre > l.pre;
}

async function checkAndroidUpdate(): Promise<UpdateHandle | null> {
  const current = await tauriGetVersion();
  const manifestUrl = await androidManifestUrl();
  let rawBytes: Uint8Array;
  let signature: string;
  try {
    const [mRes, sRes] = await Promise.all([
      fetch(manifestUrl, { cache: "no-store" }),
      fetch(`${manifestUrl}.sig`, { cache: "no-store" }),
    ]);
    if (!mRes.ok || !sRes.ok) return null;
    rawBytes = new Uint8Array(await mRes.arrayBuffer());
    signature = (await sRes.text()).trim();
  } catch {
    return null;
  }
  // Authenticate the EXACT manifest bytes (the signature is over the raw file)
  // before trusting any version, url, or hash inside it.
  if (!verifyEd25519Detached(ANDROID_MANIFEST_PUBKEY, signature, rawBytes)) return null;

  let manifest: AndroidManifest;
  try {
    manifest = JSON.parse(new TextDecoder().decode(rawBytes)) as AndroidManifest;
  } catch {
    return null;
  }
  if (!manifest?.version || !isNewerVersion(manifest.version, current)) return null;
  // Pick the APK built for THIS device's ABI, and decline rather than guess: an
  // APK whose native library the CPU cannot load passes the signature and hash
  // checks, so nothing else here would catch it, and a self-update that breaks
  // the app is worse than one that does nothing.
  //
  // `client_abi` reports the ABI this build was compiled for, which IS the ABI
  // running. The universal fallback stays while fat APKs are published; there is
  // deliberately no "first entry" fallback.
  let abi: string | null = null;
  try {
    abi = (await invoke<string | null>("client_abi")) ?? null;
  } catch {
    // An older shell without the command. Fall through to universal only.
  }
  const entry = (abi ? manifest.abis[abi] : undefined) ?? manifest.abis["android-universal"];
  if (!entry?.url || !entry?.sha256) return null;
  const expectedSha = entry.sha256.toLowerCase();

  return {
    version: manifest.version,
    notes: manifest.notes,
    async downloadAndInstall(onProgress) {
      const res = await fetch(entry.url);
      if (!res.ok || !res.body) throw new Error(`APK download failed: ${res.status}`);
      const total = Number(res.headers.get("content-length")) || undefined;
      onProgress?.({ kind: "Started", total });
      // Stream the APK, reporting real per-chunk progress instead of buffering
      // silently and emitting one synthetic event.
      const reader = res.body.getReader();
      const chunks: Uint8Array[] = [];
      let received = 0;
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        chunks.push(value);
        received += value.length;
        onProgress?.({ kind: "Progress", chunk: value.length });
      }
      const bytes = new Uint8Array(received);
      let offset = 0;
      for (const c of chunks) {
        bytes.set(c, offset);
        offset += c.length;
      }
      // Verify the download against the signed manifest's hash before staging.
      const digest = await sha256Hex(bytes);
      if (!timingSafeEqualHex(digest, expectedSha)) {
        throw new Error("downloaded APK does not match the signed manifest hash");
      }
      const path = `${await tauriTempDir()}/loca-${manifest.version}.apk`;
      await tauriWriteFile(path, bytes);
      onProgress?.({ kind: "Finished" });
      // Opening the APK hands off to Android's package installer (needs the
      // REQUEST_INSTALL_PACKAGES permission); the user confirms the install.
      await openPath(path);
    },
    close: () => Promise.resolve(),
  };
}

// --- The Core inside this app ----------------------------------------------
//
// The phone IS a Core: `loca-core` is linked into the app and driven in process
// through the `core_*` commands. There is no loopback listener, so none of these
// carry a TLS mode, a pin, or a socket URL. Keeping every `invoke` / `listen`
// for it here is what lets `lib/core/local-transport.ts` stay import-free.
const localCore: LocalCore = {
  start: () => invoke<LocalCoreInfo>("core_start"),
  running: () => invoke<boolean>("core_running"),
  http: (req: LocalCoreRequest) =>
    invoke<LocalCoreReply>("core_http", {
      url: req.url,
      method: req.method,
      headers: req.headers,
      bodyB64: req.bodyB64,
    }),
  wsOpen: (wsId) => invoke("core_ws_open", { wsId }),
  wsSend: (wsId, data) => invoke("core_ws_send", { wsId, data }),
  wsClose: (wsId) => invoke("core_ws_close", { wsId }),
  async subscribeWs(wsId, handlers) {
    // Three events, not four: there is no `core://ws/<id>/error`, because
    // registering with the frame bus cannot fail. The close payload carries the
    // close code, which nothing on this side acts on (a local close is always
    // real, so the response is the same whatever the code says).
    const unlisteners: UnlistenFn[] = await Promise.all([
      listen(`core://ws/${wsId}/open`, () => handlers.onOpen()),
      listen<string>(`core://ws/${wsId}/message`, (e) => handlers.onMessage(e.payload)),
      listen(`core://ws/${wsId}/close`, () => handlers.onClose()),
    ]);
    return () => {
      for (const u of unlisteners) u();
    };
  },
};

/** Reject for a host feature that genuinely has no Android equivalent. The UI
 *  gates these affordances on `UiContext.platform`, so a thrown error here is a
 *  belt-and-suspenders guard, not an expected path. */
function notSupported(feature: string): Promise<never> {
  return Promise.reject(new Error(`${feature} is not supported on mobile`));
}

/** A subscription stub that never fires, returning an inert detach. Used for
 *  desktop-only event streams (tray/hotkey/monitor changes). */
async function inertSubscription(): Promise<() => void> {
  return () => {};
}

/** iOS has no hardware back key, so a left-edge swipe drives the same
 *  back-handler stack the in-app back buttons use. Fires `cb` once per gesture
 *  that starts within the left screen edge and travels decisively rightward;
 *  a mostly-vertical drag (a scroll that happened to start near the edge) is
 *  ignored so it is not swallowed. */
function subscribeEdgeSwipeBack(cb: () => void): () => void {
  const EDGE_PX = 24; // start zone measured from the left screen edge
  const THRESHOLD_PX = 64; // horizontal travel that commits the gesture
  let startX = 0;
  let startY = 0;
  let tracking = false;
  const onStart = (e: TouchEvent): void => {
    const t = e.touches[0];
    if (!t || t.clientX > EDGE_PX) return;
    tracking = true;
    startX = t.clientX;
    startY = t.clientY;
  };
  const onMove = (e: TouchEvent): void => {
    if (!tracking) return;
    const t = e.touches[0];
    if (!t) return;
    const dx = t.clientX - startX;
    const dy = t.clientY - startY;
    if (dx > THRESHOLD_PX && Math.abs(dx) > Math.abs(dy)) {
      tracking = false;
      cb();
    }
  };
  const onEnd = (): void => {
    tracking = false;
  };
  document.addEventListener("touchstart", onStart, { passive: true });
  document.addEventListener("touchmove", onMove, { passive: true });
  document.addEventListener("touchend", onEnd, { passive: true });
  document.addEventListener("touchcancel", onEnd, { passive: true });
  return () => {
    document.removeEventListener("touchstart", onStart);
    document.removeEventListener("touchmove", onMove);
    document.removeEventListener("touchend", onEnd);
    document.removeEventListener("touchcancel", onEnd);
  };
}

const impl: Platform = {
  // Pinned HTTP + WebSocket to a paired core; shared with the desktop impl
  // since both call the same Rust commands (see lib/platform/shared.ts). LAN
  // discovery is desktop-only (the Rust command isn't registered on mobile and
  // interface enumeration is out of scope), so override it with an empty sweep.
  net: { ...net, discoverCores: () => Promise.resolve([]) },
  localCore,
  windowing: {
    // A single fullscreen activity: there is no floating window to move, size,
    // or hide. show/hide/position resolve as no-ops so the boot path (which
    // positions + shows the desktop bubble) proceeds unchanged.
    show: () => Promise.resolve(),
    hide: () => Promise.resolve(),
    toggle: () => Promise.resolve(),
    requestHide: () => Promise.resolve(),
    position: () => Promise.resolve(),
    isVisible: () => Promise.resolve(true),
    async subscribeVisibility(cb) {
      // Map foreground/background to the WebView's document visibility so
      // foreground-gated work (e.g. VAD) can pause when the app is backgrounded.
      const handler = () => cb(document.visibilityState === "visible");
      document.addEventListener("visibilitychange", handler);
      return () => document.removeEventListener("visibilitychange", handler);
    },
    outerSize: () => Promise.resolve({ width: 0, height: 0 }),
    outerPosition: () => Promise.resolve({ x: 0, y: 0 }),
    setOuterSize: () => Promise.resolve(),
    setOuterPosition: () => Promise.resolve(),
    currentMonitor: () => Promise.resolve(null),
    subscribeHideRequested: inertSubscription,
    subscribeMonitorChanged: inertSubscription,
  },
  backButton: {
    // Android's app plugin emits a `back-button` event for every system back
    // press (TauriActivity ships handleBackNavigation=false, so the JS layer owns
    // back). iOS has no hardware back, so a left-edge swipe feeds the same
    // handler registry, which decides what each back does.
    async subscribe(cb) {
      if (osPlatform() === "ios") return subscribeEdgeSwipeBack(cb);
      const listener = await onBackButtonPress(() => cb());
      return () => void listener.unregister();
    },
    // Android leaves the app on the final root double-back via app.hide() (the
    // only background path that needs no extra plugin; the process plugin has no
    // Android support). iOS apps must not programmatically exit or move
    // themselves to the background (App Store rejects it), so leaving is a no-op
    // there. Best-effort: never throw out of a back-button handler.
    async exit() {
      if (osPlatform() === "ios") return;
      try {
        await tauriHide();
      } catch {
        // No-op: a failed background must not crash the back handler.
      }
    },
    // Only Android leaves the app on a root back; on iOS the OS home gesture owns
    // that, so a root edge-swipe is inert rather than arming a dead exit hint.
    canExit: () => osPlatform() === "android",
  },
  autostart: {
    isEnabled: () => Promise.resolve(false),
    setEnabled: () => notSupported("Launch on login"),
    wasAutostarted: () => Promise.resolve(false),
  },
  capture: {
    // Screen capture is replaced on mobile by attaching photos/files; the
    // region-overlay flow has no mobile analogue.
    monitors: () => Promise.resolve([]),
    captureMonitor: () => notSupported("Screen capture"),
    setRegionTarget: () => Promise.resolve(),
    getRegionTarget: () => Promise.resolve("primary"),
    showRegionOverlay: () => notSupported("Region capture"),
    hideRegionOverlay: () => Promise.resolve(),
    subscribeRegionResult: inertSubscription,
  },
  audio: {
    // No app-level OS volume control on Android; VAD listens without ducking.
    getSystemVolume: () => Promise.resolve(100),
    setSystemVolume: () => Promise.resolve(),
    restoreSystemVolume: () => Promise.resolve(),
  },
  fonts: {
    // Mobile has no font-enumeration API exposed here, so offer a curated list
    // of families that resolve on the OS (the system default plus widely bundled
    // faces) instead of an empty picker. "default" maps to the system stack via
    // the appearance settings.
    list: () =>
      Promise.resolve(
        osPlatform() === "ios"
          ? ["Helvetica Neue", "Avenir Next", "Georgia", "Menlo", "monospace"]
          : ["Roboto", "Noto Sans", "Noto Serif", "Droid Sans Mono", "monospace"],
      ),
  },
  process: {
    selfMetrics: () => invoke("get_self_metrics"),
  },
  clientStorage: {
    tree: () => invoke("get_client_storage"),
    truncateActiveLog: () => invoke("truncate_client_log"),
  },
  shortcuts: {
    // No OS-level global hotkeys or window-scoped accelerators on Android; the
    // mobile UI offers on-screen buttons instead.
    setBinding: () => Promise.resolve(),
    validate: () => notSupported("Global shortcuts"),
    setPttConfig: () => Promise.resolve(),
    subscribeEvents: inertSubscription,
    setInputBindings: () => Promise.resolve(),
    subscribeInputEvents: inertSubscription,
  },
  clientFiles: {
    read: (file) => invoke("read_client_file", { file }),
    write: (file, data) => invoke("write_client_file", { file, data }),
  },
  snippetFiles: {
    readAll: () => invoke("read_client_snippets"),
    write: (name, data) => invoke("write_client_snippet", { name, data }),
    delete: (name) => invoke("delete_client_snippet", { name }),
  },
  keychain: {
    set: (coreId, token) => invoke("keychain_set_token", { coreId, token }),
    get: (coreId) => invoke("keychain_get_token", { coreId }),
    delete: (coreId) => invoke("keychain_delete_token", { coreId }),
  },
  pairing: {
    // This device hosts a Core in process rather than as a binary on a port, so
    // the desktop-shaped questions get an embedded Core's answers. No admin token
    // and no boot breadcrumb (both are files a supervised process writes), and no
    // pairing codes, since nothing can pair to a Core with no listener.
    readAdminToken: () => Promise.resolve(null),
    readLocalCoreBootError: () => Promise.resolve(null),
    // Always present: the Core is compiled into this app, so it cannot be
    // missing the way an uninstalled desktop core can.
    isLocalCoreInstalled: () => Promise.resolve(true),
    isLocalCoreRunning: () => localCore.running(),
    startLocalCore: () => localCore.start().then(() => true),
    // Nothing to stop. `StartOnce` cannot be un-started, and an embedded Core
    // lives exactly as long as the app does, so this is a no-op rather than a
    // rejection: callers (the tray item, the CoreBar pill) are desktop-only
    // affordances that never reach it here.
    stopLocalCore: () => Promise.resolve(),
    // No origin, because there is no listener. See the interface doc for why
    // this stays `""` rather than becoming null.
    localCoreBaseUrl: () => Promise.resolve(""),
    localSidecarPorts: () => Promise.resolve({ llm: 0, stt: 0 }),
    // Dev-only onboarding autofill, the mobile analogue of the desktop
    // `--core-url` / `--pairing-code` arguments (a mobile build has no argv).
    // `deno task dev:android` bakes VITE_DEV_CORE_URL (the host core at a
    // device-reachable address) and VITE_DEV_PAIRING_CODE into the dev bundle;
    // without the URL env the address is derived from the dev-server host on the
    // channel's core port. Both fields are prefilled, never locked.
    launchPrefill: async () => {
      if (!import.meta.env.DEV) return null;
      const pairingCode = (import.meta.env.VITE_DEV_PAIRING_CODE as string | undefined)?.trim();
      const coreUrlEnv = (import.meta.env.VITE_DEV_CORE_URL as string | undefined)?.trim();
      if (coreUrlEnv) {
        return { coreUrl: coreUrlEnv, pairingCode: pairingCode || undefined };
      }
      const host = globalThis.location?.hostname;
      if (!host) return null;
      let channel = "stable";
      try {
        channel = (await invoke<string>("client_channel")) || "stable";
      } catch {
        /* default to the stable port */
      }
      const port = 7800 + (channel === "dev" ? 20 : channel === "latest" ? 10 : 0);
      return { coreUrl: `https://${host}:${port}`, pairingCode: pairingCode || undefined };
    },
  },
  fileConvert: {
    // File-to-markdown uses desktop-only Rust crates; mobile attachments are
    // wired through the core in a later pass.
    toMarkdown: () => notSupported("File conversion"),
    toMarkdownFromPath: () => notSupported("File conversion"),
  },
  resolvePath: (path) => invoke("resolve_path", { path }),
  openExternal: (url) => openUrl(url),
  // No OS file-manager reveal on Android; treat as a no-op (callers already
  // gate the affordance on same-device availability).
  revealPath: () => Promise.resolve(),
  updater: {
    getVersion: () => tauriGetVersion(),
    // Android self-hosts updates: it compares the app version against the
    // R2-hosted android.json and hands a newer APK to the package installer (see
    // above). iOS has no OTA self-install path, so updates come from the App
    // Store and there is nothing to check here.
    check: () => (osPlatform() === "ios" ? Promise.resolve(null) : checkAndroidUpdate()),
    // Android hands the newer APK to the OS package installer (a working
    // self-install); iOS never surfaces an update (check returns null), so
    // in-place install is always the right path on mobile.
    canSelfInstall: () => Promise.resolve(true),
    // No in-process relaunch on mobile: Android's package installer restarts the
    // app, and iOS is relaunched from the home screen.
    relaunch: () => notSupported("Relaunch"),
  },
  fs: {
    readFile: (path) => tauriReadFile(path),
    writeFile: (path, bytes) => tauriWriteFile(path, bytes),
    remove: (path) => remove(path),
    tempDir: () => tauriTempDir(),
    join: (...segments) => tauriJoin(...segments),
  },
  dialog: {
    async openFilePicker(opts) {
      // The Android dialog plugin returns content URIs rather than absolute
      // paths; attachment ingestion routes those through bytes, not paths.
      const picked = await openDialog({
        multiple: opts?.multiple ?? false,
        filters: opts?.filters,
      });
      if (picked === null) return [];
      return Array.isArray(picked) ? picked : [picked];
    },
  },
  cursor: {
    // No pointer on touch; the bubble click-through machinery is gated off.
    getPosition: () => Promise.resolve({ x: 0, y: 0 }),
    setClickthrough: () => Promise.resolve(),
  },
  menu: {
    // Native desktop context menus are replaced by an in-app action sheet. The
    // existing menu builders call this with the same items; the host renders a
    // shared ActionSheet and resolves with the chosen id, so every context menu
    // works on touch once a long-press trigger invokes its builder.
    showContextMenu: (items) => presentActionSheet(items),
  },
  tray: {
    // No status-bar icon on a single-activity mobile shell, and no on-device
    // core to start or stop.
    setCoreState: () => Promise.resolve(),
    subscribeCoreAction: () => Promise.resolve(() => {}),
  },
  monitors: {
    primary: () => Promise.resolve(null),
    available: () => Promise.resolve([]),
  },
  logging: {
    log(level, scope, message) {
      void invoke("client_log", { level, scope, message }).catch(() => {});
    },
  },
};
