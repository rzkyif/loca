// Platform abstraction: every host-only feature the Svelte UI uses goes through
// this interface, so the same code runs under each native shell (`tauri.ts`
// desktop, `mobile.ts` android), selected at boot by `select.ts`.
//
// IMPORTANT: nothing under `src/ui/` outside `lib/platform/` may import
// `@tauri-apps/*` (enforced by an oxlint rule). To add a feature: declare the
// method on `Platform` below, implement it in `tauri.ts` and `mobile.ts`
// (sharing helpers via `shared.ts`), cover it in `src/ui/test/platform-stub.ts`,
// then call `platform().<namespace>.<method>()`.

import type { StorageTree } from "@loca/shared";

// Mirrors the Rust `list_capture_monitors` shape. The physical-pixel bounds let
// region capture match the active monitor against Tauri's `currentMonitor()`
// position rather than relying on names matching.
export interface MonitorInfo {
  id: string;
  name: string;
  isPrimary: boolean;
  x: number;
  y: number;
  width: number;
  height: number;
}

// --- Client files ----------------------------------------------------------

// The fixed-name JSON stores under ~/.loca/<channel>/client/. Must match the
// Rust ClientFile serde variants (lowercase) exactly.
export type ClientFileName = "settings" | "cores";

// --- Context menu --------------------------------------------------------

export type ContextMenuItem =
  | { id: string; label: string; enabled?: boolean; checked?: boolean }
  | { separator: true };

// --- Updater --------------------------------------------------------------

/** Per-update handle from `updater.check()`. Stateful, so callers MUST `.close()`
 *  (or call `downloadAndInstall`, which closes implicitly). */
export interface UpdateHandle {
  /** Version available for install (semver). */
  readonly version: string;
  /** Optional changelog / release notes from the manifest. */
  readonly notes?: string;
  /** Download + install. Resolves once the staging swap is queued; the
   *  OS-level relaunch is separate. */
  downloadAndInstall(
    onProgress?: (
      event:
        | { kind: "Started"; total?: number }
        | { kind: "Progress"; chunk: number }
        | { kind: "Finished" },
    ) => void,
  ): Promise<void>;
  /** Release the underlying handle without installing. */
  close(): Promise<void>;
}

export interface ShortcutBindings {
  /** Set or clear (null) the OS-level global shortcut binding. */
  setBinding(accelerator: string | null): Promise<void>;
  /** Probe registrability without keeping it, to surface "already taken". */
  validate(accelerator: string): Promise<void>;
  /** Configure how Rust classifies a window-shortcut press. Rust owns the
   *  tap-vs-hold timing so it can't depend on the release event reaching a
   *  congested webview in time. Call on attach and on any mode/duration change. */
  setPttConfig(pushToTalk: boolean, holdMs: number): Promise<void>;
  /** `onPressed` fires on every press; the rest only in push-to-talk mode, per
   *  Rust's tap-vs-hold verdict. Returns a detach. */
  subscribeEvents(handlers: {
    onPressed: () => void;
    onTap?: () => void;
    onHoldStart?: () => void;
    onHoldEnd?: () => void;
  }): Promise<() => void>;
  /** Register [logical-id, accelerator] shortcuts that fire only while the input
   *  surface is focused. Pass `[]` to clear. */
  setInputBindings(bindings: Array<[string, string]>): Promise<void>;
  /** Subscribe to the shortcuts registered via `setInputBindings`. Returns a
   *  detach. */
  subscribeInputEvents(handlers: {
    onAttachFile?: () => void;
    onCaptureScreen?: () => void;
    onCaptureRegion?: () => void;
  }): Promise<() => void>;
}

// --- Networking ----------------------------------------------------------
//
// All traffic to a paired core goes through `platform().net`, which terminates
// TLS below the webview in Rust: a plain webview fetch cannot pin a self-signed
// cert or choose its verifier. Every request states a `TlsMode`.

/** TLS trust posture for one request. `pin` enforces the stored SPKI pin (a
 *  self-signed core); `webpki` does public-CA validation (a core behind an HTTPS
 *  proxy); `capture` accepts any cert and reports its pin, for the unpaired
 *  pairing/discovery probe ONLY. There is no accept-any default. */
export type TlsMode = "pin" | "webpki" | "capture";

export interface NetRequest {
  url: string;
  method?: string;
  headers?: Record<string, string>;
  /** Request body. Binary (multipart, audio) is passed as bytes. */
  body?: string | Uint8Array;
  /** Trust mode for this request (required). */
  mode: TlsMode;
  /** Expected SPKI pin to enforce (base64 SHA-256). Required when `mode` is
   *  `pin`, ignored otherwise. */
  pin?: string;
}

export interface NetResponse {
  status: number;
  headers: Record<string, string>;
  body: Uint8Array;
  /** The pin the server presented, when `mode` was `capture`. */
  capturedPin?: string;
}

/** A core found by `net.discoverCores()`. The pin is display-only; real trust is
 *  established by the PAKE + pin-binding pairing flow. */
export interface DiscoveredCore {
  baseUrl: string;
  version: string;
  pin: string;
}

/** A pinned WebSocket. Mirrors the slice of `WebSocket` that CoreClient uses. */
export interface NetSocket {
  send(data: string): void;
  close(): void;
  onOpen(cb: () => void): void;
  onMessage(cb: (data: string) => void): void;
  onClose(cb: () => void): void;
  /** `reason` carries the connect-failure message when available. */
  onError(cb: (reason?: string) => void): void;
}

/** Severity levels for the client logger (lib/util/log.ts). */
export type LogLevel = "debug" | "info" | "warn" | "error";

// --- The Core inside this app ---------------------------------------------

/** One request into the embedded Core. `url` may be whole or a bare path+query;
 *  `bodyB64` is base64 so bytes survive the IPC hop. */
export interface LocalCoreRequest {
  url: string;
  method: string;
  headers: Record<string, string>;
  bodyB64: string | null;
}

/** One reply from the embedded Core. An HTTP-level failure (401, 404, 500)
 *  arrives HERE, not as a rejection, exactly as it would over a socket. */
export interface LocalCoreReply {
  status: number;
  headers: Record<string, string>;
  bodyB64: string;
}

/** What the embedded Core reports about itself once started. */
export interface LocalCoreInfo {
  version: string;
  channel: string;
  root: string;
}

/** The Core compiled into this app, reached in process.
 *
 *  There is deliberately no socket and no TLS: `127.0.0.1` is not app-private on
 *  Android, so binding a port would expose the Core to every installed app.
 *  `mode` / `pin` / `capturedPin` are therefore absent rather than ignored, and
 *  there is no `error` event: registering with the frame bus cannot fail. */
export interface LocalCore {
  /** Boot the Core if it is not running. Idempotent; a failed start is
   *  retryable rather than cached. */
  start(): Promise<LocalCoreInfo>;
  /** Whether it has been started. Never starts it. */
  running(): Promise<boolean>;
  http(req: LocalCoreRequest): Promise<LocalCoreReply>;
  /** Attach a client connection under `wsId`. Re-using a live id silently
   *  replaces the previous connection, so callers pass a fresh id. */
  wsOpen(wsId: string): Promise<void>;
  wsSend(wsId: string, data: string): Promise<void>;
  wsClose(wsId: string): Promise<void>;
  /** Subscribe to one connection's frames. Register BEFORE `wsOpen`: `open` is
   *  emitted before any seed frame, so a late listener loses it. */
  subscribeWs(
    wsId: string,
    handlers: { onOpen(): void; onMessage(data: string): void; onClose(): void },
  ): Promise<() => void>;
}

export interface Platform {
  // Network access to a paired core, TLS pinned below the webview.
  net: {
    fetch(req: NetRequest): Promise<NetResponse>;
    connectWebSocket(
      url: string,
      opts: { mode: "pin" | "webpki"; pin?: string },
    ): Promise<NetSocket>;
    /** Sweep this machine's /24(s) plus loopback for cores answering
     *  /api/v1/health, deduped by cert pin. Desktop only; [] on mobile. */
    discoverCores(): Promise<DiscoveredCore[]>;
  };
  /** The Core compiled into this app, or `null` where the app embeds none (every
   *  desktop build). A property of the BUILD, never of the selected core. */
  localCore: LocalCore | null;
  // Window control.
  windowing: {
    show(): Promise<void>;
    hide(): Promise<void>;
    toggle(): Promise<void>;
    requestHide(): Promise<void>;
    position(args: {
      monitorId?: string;
      alignment?: "left" | "center" | "right";
      /** Optional logical-pixel width to apply along with the position. */
      width?: number;
    }): Promise<void>;
    /** True when the main window is currently shown. */
    isVisible(): Promise<boolean>;
    /** Fires on every show/hide, including Rust's tray / hotkey handlers. */
    subscribeVisibility(cb: (visible: boolean) => void): Promise<() => void>;
    /** Read the main window's current outer size in physical pixels. */
    outerSize(): Promise<{ width: number; height: number }>;
    /** Outer position in physical pixels, relative to the primary monitor. */
    outerPosition(): Promise<{ x: number; y: number }>;
    /** Set the main window's size in physical pixels. */
    setOuterSize(size: { width: number; height: number }): Promise<void>;
    /** Set the main window's position in physical pixels. */
    setOuterPosition(pos: { x: number; y: number }): Promise<void>;
    /** Read the monitor the main window is currently positioned on. */
    currentMonitor(): Promise<MonitorInfo | null>;
    /** Rust-originated "hide me" requests (tray click, hotkey). Returns a detach. */
    subscribeHideRequested(cb: () => void): Promise<() => void>;
    /** Monitor arrangement changed (DPI/scale, plug/unplug). Returns a detach. */
    subscribeMonitorChanged(cb: () => void): Promise<() => void>;
  };
  // Android hardware / gesture back button, inert on desktop. The back-handler
  // registry (state/back.svelte.ts) decides what each press does.
  backButton: {
    /** Fire on every mobile back gesture. Returns a detach. Inert on desktop. */
    subscribe(cb: () => void): Promise<() => void>;
    /** Leave the app. Android backgrounds the task; no-op on desktop and iOS
     *  (Apple forbids a programmatic exit). */
    exit(): Promise<void>;
    /** Whether a root back gesture should leave the app. Android only: on iOS the
     *  OS home gesture owns leaving, so a root back is inert. */
    canExit(): boolean;
  };
  // OS login entry ("start Loca when I log in").
  autostart: {
    isEnabled(): Promise<boolean>;
    setEnabled(enabled: boolean): Promise<void>;
    /** True when this run was launched by the login entry rather than the user. */
    wasAutostarted(): Promise<boolean>;
  };
  // Screen capture. Region capture is `showRegionOverlay` + a Rust event (the
  // overlay draws the selection and emits the cropped PNG); there is no direct
  // "capture a region" call.
  capture: {
    monitors(): Promise<MonitorInfo[]>;
    captureMonitor(monitorId: string): Promise<string>; // base64 PNG
    setRegionTarget(monitorId: string): Promise<void>;
    getRegionTarget(): Promise<string>;
    showRegionOverlay(): Promise<string>; // returns xcap monitor id
    hideRegionOverlay(): Promise<void>;
    /** Overlay result: base64 PNG on a successful drag, null on ESC / cancel. */
    subscribeRegionResult(cb: (payload: string | null) => void): Promise<() => void>;
  };
  // System audio.
  audio: {
    getSystemVolume(): Promise<number>;
    setSystemVolume(percent: number): Promise<void>;
    restoreSystemVolume(): Promise<void>;
  };
  // Installed fonts.
  fonts: { list(): Promise<string[]> };
  // This process's own resource usage, for the Services field's "Main
  // Application" row.
  process: {
    selfMetrics(): Promise<{ pid: number; rssMb: number; cpuPct: number }>;
  };
  // The client's on-disk storage tree (settings + logs) for the "Client →
  // Storage" usage field. Read-only; deletes go via fs.remove + clientFiles.
  clientStorage: {
    tree(): Promise<StorageTree>;
    /** Empty the active client.log in place. Rotated backups go via fs.remove. */
    truncateActiveLog(): Promise<void>;
  };
  // Global / input shortcuts.
  shortcuts: ShortcutBindings;
  // Per-concern client JSON stores under ~/.loca/<channel>/client/:
  // settings.json (settingsState) and cores.json (lib/core/cores.ts). One owner
  // per file, so no cross-module read-modify-write.
  clientFiles: {
    read(file: ClientFileName): Promise<Record<string, unknown>>;
    write(file: ClientFileName, data: Record<string, unknown>): Promise<void>;
  };
  // Per-snippet JSON files under ~/.loca/<channel>/client/snippets/. The
  // directory listing IS the registry, so a snippet can be shared by copying its
  // file into the folder and rescanning.
  snippetFiles: {
    /** Every parseable snippet file, keyed by filename stem. */
    readAll(): Promise<Record<string, Record<string, unknown>>>;
    write(name: string, data: Record<string, unknown>): Promise<void>;
    delete(name: string): Promise<void>;
  };
  // OS keychain for paired-core bearer tokens.
  keychain: {
    set(coreId: string, token: string): Promise<void>;
    get(coreId: string): Promise<string | null>;
    delete(coreId: string): Promise<void>;
  };
  // Pairing helpers.
  pairing: {
    readAdminToken(): Promise<string | null>;
    /** The local core's last fatal boot-failure reason, or null when it came up
     *  cleanly, so the pair flow can explain an opaque connection failure. */
    readLocalCoreBootError(): Promise<string | null>;
    /** Whether the installer planted ~/.loca/core/bin/loca-core. Gates the
     *  first-boot silent pair and the on-demand spawn. */
    isLocalCoreInstalled(): Promise<boolean>;
    /** Whether the local core's port answers. Splits "stopped" (offer to start)
     *  from "up but unreachable" (keep reconnecting) for the Stopped pill. */
    isLocalCoreRunning(): Promise<boolean>;
    /** Start the local core through whatever supervisor owns it. Returns true
     *  when one was started; an unsupervised core is stopped again on quit. */
    startLocalCore(): Promise<boolean>;
    /** Stop the local core through its supervisor when it has one (a plain kill
     *  would be undone by launchd's KeepAlive). Idempotent. */
    stopLocalCore(): Promise<void>;
    /** Loopback base URL of this channel's local core, on the channel-aware port
     *  (stable 7800, latest 7810, …).
     *
     *  EMPTY STRING where there is no such URL, which is every build whose Core
     *  is embedded: an in-process Core has no origin. Not widened to
     *  `string | null` because every consumer runs it through `new URL(...)`,
     *  which throws on `""` and `null` alike. */
    localCoreBaseUrl(): Promise<string>;
    /** This channel's default sidecar ports, used when the paired core has not
     *  overridden llm.port / stt.port. */
    localSidecarPorts(): Promise<{ llm: number; stt: number }>;
    /** Optional `--core-url` / `--pairing-code` launch arguments prefilling the
     *  "On another computer" onboarding fields. Null when neither was given. */
    launchPrefill(): Promise<{ coreUrl?: string; pairingCode?: string } | null>;
  };
  // File-to-markdown conversion (desktop uses Rust crates).
  fileConvert: {
    toMarkdown(file: File): Promise<string>;
    /** Convert a file already on disk, for the paste / dialog-pick flows. */
    toMarkdownFromPath(absPath: string): Promise<string>;
  };
  // Path expansion (~ → home).
  resolvePath(path: string): Promise<string>;
  // Default Downloads folder.
  openExternal(url: string): Promise<void>;
  // Reveal a file in the OS file manager, selecting it. Only meaningful for a
  // file on this same device; callers gate on that before offering it.
  revealPath(absPath: string): Promise<void>;
  // Tauri auto-updater, wrapped so UpdateButton need not import the plugin.
  updater: {
    /** Current installed version (from Tauri's app metadata). */
    getVersion(): Promise<string>;
    /** Check the update endpoint. Null when none is available. */
    check(): Promise<UpdateHandle | null>;
    /** Whether an update can be installed in place. False on a non-AppImage Linux
     *  install, where callers open the download page instead. */
    canSelfInstall(): Promise<boolean>;
    /** Relaunch the desktop client process. */
    relaunch(): Promise<void>;
  };
  // Filesystem ops for the drag-drop / attachment flows.
  fs: {
    readFile(path: string): Promise<Uint8Array>;
    writeFile(path: string, bytes: Uint8Array): Promise<void>;
    remove(path: string): Promise<void>;
    tempDir(): Promise<string>;
    join(...segments: string[]): Promise<string>;
  };
  // Native file picker. Returns absolute paths, or [] on cancel.
  dialog: {
    openFilePicker(opts?: {
      multiple?: boolean;
      filters?: Array<{ name: string; extensions: string[] }>;
    }): Promise<string[]>;
  };
  // Cursor introspection + click-through toggling, so the bubble window's
  // transparent regions pass mouse events to whatever is behind.
  cursor: {
    /** Cursor position in physical pixels, relative to the primary monitor. */
    getPosition(): Promise<{ x: number; y: number }>;
    /** When true, mouse events pass through transparent pixels to the desktop. */
    setClickthrough(enabled: boolean): Promise<void>;
  };
  // Native context menu for right-click message actions.
  menu: {
    /** Show at the cursor; resolves with the chosen item's id, or null if
     *  dismissed. */
    showContextMenu(items: ContextMenuItem[]): Promise<string | null>;
  };
  // The status-bar icon's Start / Stop Core item. The UI pushes its state rather
  // than the host polling, and the item only emits an intent: the UI performs the
  // start / stop, so the tray and the CoreBar's Stopped pill run the same code.
  // No tray on mobile.
  tray: {
    /** Enable (`installed`) and label (`running`) the Start / Stop Core item. */
    setCoreState(installed: boolean, running: boolean): Promise<void>;
    /** Listen for a Start / Stop Core click. Resolves to an unsubscribe fn. */
    subscribeCoreAction(cb: (action: "start" | "stop") => void): Promise<() => void>;
  };
  // Monitor enumeration for Settings + the window-position math. Distinct from
  // `capture.monitors` (xcap-based): these come from Tauri's window module and
  // align with `windowing.position`.
  monitors: {
    primary(): Promise<MonitorInfo | null>;
    available(): Promise<MonitorInfo[]>;
  };
  // Structured logging routed to Rust so lines reach the dev terminal and the
  // persisted client.log. Callers use getLogger(scope), not this directly.
  logging: {
    /** Fire-and-forget. `scope` is separate so Rust renders it as its own column. */
    log(level: LogLevel, scope: string, message: string): void;
  };
}

let _impl: Platform | null = null;

export function setPlatform(p: Platform): void {
  _impl = p;
}

export function platform(): Platform {
  if (!_impl) throw new Error("platform() called before setPlatform()");
  return _impl;
}
