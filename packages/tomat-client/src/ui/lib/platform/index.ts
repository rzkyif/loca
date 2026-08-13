// Platform abstraction: every host-only feature the Svelte UI uses goes
// through this interface so the same code can run under each native shell
// (`tauri.ts` desktop, `mobile.ts` android) with its own impl, selected at
// boot by `select.ts` from the running OS.
//
// IMPORTANT: NOTHING under packages/tomat-client/src/ui/ outside of
// `lib/platform/` may import `@tauri-apps/*`. For components, state stores,
// and shared modules, all platform-specific calls go through `platform()`.
// This is enforced by an oxlint rule (see .oxlintrc.json). To add a new
// platform-specific feature: add the method to the `Platform` interface
// below, implement it in `tauri.ts` and `mobile.ts` (sharing helpers via
// `shared.ts`), cover it in the `src/ui/test/platform-stub.ts` fixture, then
// call `platform().<namespace>.<method>()` from the consumer. There is no web
// client, so there is no browser implementation.

import type { StorageTree } from "@tomat/shared";

// Mirrors the Rust `list_capture_monitors` return shape so the
// `lib/capture/capture.ts` flow can cast without information loss. Physical
// pixel bounds (x, y, width, height) let the region-capture flow match the
// active monitor against Tauri's `currentMonitor()` position without
// relying on names matching.
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

// The fixed-name JSON stores under ~/.tomat/<channel>/client/. Values must
// match the Rust ClientFile serde variants (lowercase) exactly.
export type ClientFileName = "settings" | "cores";

// --- Context menu --------------------------------------------------------

export type ContextMenuItem =
  | { id: string; label: string; enabled?: boolean; checked?: boolean }
  | { separator: true };

// --- Updater --------------------------------------------------------------

/** Per-update handle returned by `updater.check()`. The Tauri impl wraps
 *  `@tauri-apps/plugin-updater`'s `Update` class. Its `.downloadAndInstall`
 *  and `.close` are stateful, so the handle is a wrapper, not a plain
 *  value object. Callers MUST `.close()` (or call `downloadAndInstall`,
 *  which closes implicitly) to release the underlying resources. */
export interface UpdateHandle {
  /** Version available for install (semver). */
  readonly version: string;
  /** Optional changelog / release notes from the manifest. */
  readonly notes?: string;
  /** Run the actual download + install. Progress callback fires with
   *  per-chunk byte counts and a total when available. Resolves once the
   *  staging swap is queued; the OS-level relaunch is separate. */
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
  /** Set or clear the OS-level global shortcut binding. Passing null
   *  unregisters whatever accelerator was previously bound. */
  setBinding(accelerator: string | null): Promise<void>;
  /** Probe whether an accelerator can be registered without actually keeping
   *  it. Used by the Settings UI to surface "already taken" before persist. */
  validate(accelerator: string): Promise<void>;
  /** Configure how Rust classifies a window-shortcut press. In push-to-talk
   *  mode Rust owns the tap-vs-hold timing (a release before `holdMs` emits a
   *  `tap`, at/after it a `holdStart`/`holdEnd` pair) so the decision doesn't
   *  depend on the OS release event reaching the (possibly congested) webview
   *  in time. Call on attach and whenever the mode or hold duration changes. */
  setPttConfig(pushToTalk: boolean, holdMs: number): Promise<void>;
  /** Subscribe to the window-shortcut events. `onPressed` fires on every press;
   *  the rest fire only in push-to-talk mode per Rust's tap-vs-hold verdict.
   *  Returns a cleanup function that detaches all listeners. */
  subscribeEvents(handlers: {
    onPressed: () => void;
    onTap?: () => void;
    onHoldStart?: () => void;
    onHoldEnd?: () => void;
  }): Promise<() => void>;
  /** Register window-scoped shortcuts that fire only while the input
   *  surface is focused. Pass `[]` to clear. The tuples are
   *  [logical-id, accelerator]; the logical-id is what the matching
   *  `subscribeInputEvents` callback receives. */
  setInputBindings(bindings: Array<[string, string]>): Promise<void>;
  /** Subscribe to the window-scoped input shortcuts registered via
   *  `setInputBindings`. The Rust side emits per-id events; we expose a
   *  typed handler bundle for the three logical actions the UI cares
   *  about today. Returns a cleanup detach. */
  subscribeInputEvents(handlers: {
    onAttachFile?: () => void;
    onCaptureScreen?: () => void;
    onCaptureRegion?: () => void;
  }): Promise<() => void>;
}

// --- Networking ----------------------------------------------------------
//
// All traffic to a paired core goes through `platform().net`, which terminates
// TLS below the webview (a plain webview fetch can't pin a self-signed cert or
// choose its verifier). The desktop impl does it in Rust (reqwest +
// tokio-tungstenite + per-request rustls verifiers); mobile reuses the same
// Rust. Every request states a `TlsMode`; `pin` is base64(SHA-256(SPKI)).

/** TLS trust posture for one request, resolved below the webview in Rust.
 *  `pin` enforces the stored SPKI pin (a self-signed core the Client secures
 *  itself); `webpki` does standard public-CA validation (a core behind an HTTPS
 *  proxy); `capture` accepts any cert and reports its pin, for the unpaired
 *  pairing/discovery probe only. There is no accept-any default: every request
 *  states its mode. */
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

/** A core found on the local network by `net.discoverCores()`. The pin is the
 *  cert SPKI captured during the discovery probe, kept for display only; the
 *  real trust is re-established by the PAKE + pin-binding pairing flow. */
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
  /** `reason` carries the connect-failure message when available (e.g. from the
   *  Tauri WS error event), so callers can surface it instead of a generic one. */
  onError(cb: (reason?: string) => void): void;
}

/** Severity levels for the client logger (lib/util/log.ts). */
export type LogLevel = "debug" | "info" | "warn" | "error";

export interface Platform {
  // Network access to a paired core, with TLS certificate pinning enforced
  // below the webview. See the NetRequest/NetSocket docs above.
  net: {
    fetch(req: NetRequest): Promise<NetResponse>;
    connectWebSocket(
      url: string,
      opts: { mode: "pin" | "webpki"; pin?: string },
    ): Promise<NetSocket>;
    /** Sweep the local network for reachable cores (the pairing "ping"
     *  button). Probes each host on this machine's /24(s) plus loopback at the
     *  known core ports against the unauthenticated /api/v1/health, deduped by
     *  cert pin. Desktop only; returns [] on mobile. */
    discoverCores(): Promise<DiscoveredCore[]>;
  };
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
    /** Subscribe to main-window visibility transitions. The callback fires
     *  whenever the window shows or hides (driven by Rust's tray / hotkey
     *  handlers, not just the JS-driven show/hide calls). */
    subscribeVisibility(cb: (visible: boolean) => void): Promise<() => void>;
    /** Read the main window's current outer size in physical pixels. */
    outerSize(): Promise<{ width: number; height: number }>;
    /** Read the main window's current outer position in physical pixels
     *  relative to the primary monitor origin. */
    outerPosition(): Promise<{ x: number; y: number }>;
    /** Set the main window's size in physical pixels. */
    setOuterSize(size: { width: number; height: number }): Promise<void>;
    /** Set the main window's position in physical pixels. */
    setOuterPosition(pos: { x: number; y: number }): Promise<void>;
    /** Read the monitor the main window is currently positioned on. */
    currentMonitor(): Promise<MonitorInfo | null>;
    /** Subscribe to Rust-originated "hide me" requests (tray click,
     *  hotkey, etc.). Returns a detach. */
    subscribeHideRequested(cb: () => void): Promise<() => void>;
    /** Subscribe to the monitor-arrangement-changed event (DPI/scale
     *  change, monitor plugged/unplugged). Returns a detach. */
    subscribeMonitorChanged(cb: () => void): Promise<() => void>;
  };
  // Android hardware / gesture back button. Desktop has no analogue, so the
  // methods are inert there. `subscribe` fires on every system back press; the
  // app's back-handler registry (state/back.svelte.ts) decides what each press
  // does. `exit` leaves the app, the final step of the chat-root
  // double-back-to-exit chain.
  backButton: {
    /** Fire `cb` on every mobile back gesture (Android's hardware/system back or
     *  an iOS left-edge swipe). Returns a detach. Inert (never fires) on desktop. */
    subscribe(cb: () => void): Promise<() => void>;
    /** Leave the app (background / quit). No-op on desktop and iOS (Apple forbids
     *  a programmatic exit); Android backgrounds the task. */
    exit(): Promise<void>;
    /** Whether a back gesture at the app root should leave the app. True on
     *  Android; false on desktop (the stream never fires) and iOS (the OS home
     *  gesture owns leaving, so a root back is inert instead of a dead double-tap
     *  exit hint). */
    canExit(): boolean;
  };
  // OS login entry ("start Tomat when I log in").
  autostart: {
    isEnabled(): Promise<boolean>;
    setEnabled(enabled: boolean): Promise<void>;
    /** True when this app run was launched by the login entry rather than
     *  the user. Drives the greeting trigger's `launch` report. */
    wasAutostarted(): Promise<boolean>;
  };
  // Screen capture. Region capture goes through `showRegionOverlay` + a Rust
  // event (the overlay window draws the selection and emits the cropped PNG);
  // there is no direct "capture a region" platform call.
  capture: {
    monitors(): Promise<MonitorInfo[]>;
    captureMonitor(monitorId: string): Promise<string>; // base64 PNG
    setRegionTarget(monitorId: string): Promise<void>;
    getRegionTarget(): Promise<string>;
    showRegionOverlay(): Promise<string>; // returns xcap monitor id
    hideRegionOverlay(): Promise<void>;
    /** Subscribe to the overlay's result event. `payload` is base64 PNG on
     *  successful drag, null on ESC / cancel. */
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
  // This (client) process's own resource usage, for the Services field's
  // "Main Application" row. rssMb = resident memory in MB, cpuPct = CPU %.
  process: {
    selfMetrics(): Promise<{ pid: number; rssMb: number; cpuPct: number }>;
  };
  // The local client's on-disk storage tree (settings + logs) for the
  // "Client → Storage" usage field. Read-only; deletes/clears are done by the
  // caller via fs.remove + clientFiles.
  clientStorage: {
    tree(): Promise<StorageTree>;
    /** Empty the active client.log in place (logging continues). Rotated
     *  backups are deleted via fs.remove. */
    truncateActiveLog(): Promise<void>;
  };
  // Global / input shortcuts.
  shortcuts: ShortcutBindings;
  // Per-concern client JSON stores under ~/.tomat/<channel>/client/:
  // settings.json (sparse settings, owned by settingsState) and cores.json
  // (paired-cores registry, owned by lib/core/cores.ts). One owner per file,
  // so no cross-module read-modify-write.
  clientFiles: {
    read(file: ClientFileName): Promise<Record<string, unknown>>;
    write(file: ClientFileName, data: Record<string, unknown>): Promise<void>;
  };
  // Per-snippet JSON files under ~/.tomat/<channel>/client/snippets/. The
  // directory listing is the registry (no index file), so a snippet can be
  // shared by copying its file into the folder and rescanning.
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
    /** The local core's last fatal boot-failure reason (one line), or null when
     *  the core came up cleanly. The core writes it on a fatal startup path
     *  (port in use, missing helper, ...) and clears it once it next binds, so
     *  the pair flow can explain an otherwise-opaque connection failure. */
    readLocalCoreBootError(): Promise<string | null>;
    /** Whether ~/.tomat/core/bin/tomat-core exists (planted by the native
     *  installer). Decides both the first-boot silent pair with the
     *  installer-provided core and whether to spawn it for on-demand mode. */
    isLocalCoreInstalled(): Promise<boolean>;
    /** Whether the local core's port is answering. Distinguishes "the core is
     *  stopped" (offer to start it) from "the core is up but unreachable" (keep
     *  reconnecting), which is the split the CoreBar's Stopped pill needs. */
    isLocalCoreRunning(): Promise<boolean>;
    /** Start the local core when it isn't already responding, through whatever
     *  supervisor owns it. Returns `true` when a new core was started. An
     *  unsupervised core is recorded and stopped again when the Client quits. */
    startLocalCore(): Promise<boolean>;
    /** Stop the local core, through its supervisor when it has one (a plain kill
     *  would just be undone by launchd's KeepAlive). Idempotent. */
    stopLocalCore(): Promise<void>;
    /** Loopback base URL of this channel's local core, with the channel-aware
     *  port (stable 7800, latest 7810, …). Used by the local-core pair flow so
     *  a latest client targets the latest core. */
    localCoreBaseUrl(): Promise<string>;
    /** This channel's default local sidecar ports (llama / whisper). Used as
     *  fallbacks when the paired core hasn't overridden llm.port / stt.port so
     *  a latest client talks to the latest sidecars (7711/7712), not stable's. */
    localSidecarPorts(): Promise<{ llm: number; stt: number }>;
    /** Optional `--core-url` / `--pairing-code` launch arguments, used to
     *  prefill the "On another computer" onboarding fields. Doubles as a
     *  shareable setup command; `deno task dev` passes the dev core URL + code
     *  this way. Returns null when neither flag was given. */
    launchPrefill(): Promise<{ coreUrl?: string; pairingCode?: string } | null>;
  };
  // File-to-markdown conversion (desktop uses Rust crates).
  fileConvert: {
    toMarkdown(file: File): Promise<string>;
    /** Convert a file already on disk by absolute path. Used by the paste
     *  / dialog-pick flows which produce a path directly without going
     *  through a Blob roundtrip. */
    toMarkdownFromPath(absPath: string): Promise<string>;
  };
  // Path expansion (~ → home).
  resolvePath(path: string): Promise<string>;
  // Default Downloads folder.
  openExternal(url: string): Promise<void>;
  // Reveal a file in the OS file manager (Finder/Explorer), selecting it.
  // Only meaningful when the file is on this same device (e.g. a download from
  // a same-device core); callers gate on that before showing the affordance.
  revealPath(absPath: string): Promise<void>;
  // Tauri auto-updater. Wraps @tauri-apps/plugin-updater so UpdateButton
  // doesn't have to import the plugin directly.
  updater: {
    /** Current installed version (from Tauri's app metadata). */
    getVersion(): Promise<string>;
    /** Check the configured update endpoint. Returns null when no update
     *  is available; otherwise an UpdateHandle the caller drives. */
    check(): Promise<UpdateHandle | null>;
    /** Whether an available client update can be installed in place. False on a
     *  non-AppImage Linux install (a distro/third-party repackage or raw binary),
     *  where the Tauri updater can only replace an AppImage: callers open the
     *  download page instead of running a self-update that would fail. */
    canSelfInstall(): Promise<boolean>;
    /** Relaunch the desktop client process. */
    relaunch(): Promise<void>;
  };
  // Filesystem ops needed by drag-drop / attachment flows. Async across the
  // board to match Tauri's plugin shape.
  fs: {
    readFile(path: string): Promise<Uint8Array>;
    writeFile(path: string, bytes: Uint8Array): Promise<void>;
    remove(path: string): Promise<void>;
    tempDir(): Promise<string>;
    join(...segments: string[]): Promise<string>;
  };
  // Native file-picker dialog. Returns absolute paths chosen by the user,
  // or an empty array on cancel.
  dialog: {
    openFilePicker(opts?: {
      multiple?: boolean;
      filters?: Array<{ name: string; extensions: string[] }>;
    }): Promise<string[]>;
  };
  // Cursor introspection + click-through toggling for the floating window.
  // Used by lib/window/window.ts to make the transparent regions of
  // the bubble window pass mouse events to whatever's behind.
  cursor: {
    /** Current cursor position in physical pixels relative to the primary
     *  monitor origin. */
    getPosition(): Promise<{ x: number; y: number }>;
    /** Toggle click-through on the main window. When true, mouse events
     *  pass through transparent pixels to the desktop. */
    setClickthrough(enabled: boolean): Promise<void>;
  };
  // Native context menu, used for right-click message actions where the
  // browser menu would obscure the UI.
  menu: {
    /** Show a context menu at the current cursor and resolve with the
     *  id of the chosen item, or null if dismissed. Separator entries
     *  carry no id and are skipped when matching the resolved value. */
    showContextMenu(items: ContextMenuItem[]): Promise<string | null>;
  };
  // The status-bar icon's Start / Stop Core item. The tray menu is built once
  // by the host and mutated from here, so the UI (which already tracks the
  // local core) pushes its state rather than the host polling for it. The item
  // only ever emits an intent; the UI performs the start / stop, so the tray
  // and the CoreBar's Stopped pill run the same code. No tray on mobile.
  tray: {
    /** Enable (`installed`) and label (`running`) the Start / Stop Core item. */
    setCoreState(installed: boolean, running: boolean): Promise<void>;
    /** Listen for a Start / Stop Core click. Resolves to an unsubscribe fn. */
    subscribeCoreAction(cb: (action: "start" | "stop") => void): Promise<() => void>;
  };
  // Monitor enumeration shared between Settings (pick a monitor) and the
  // window-position math. Distinct from `capture.monitors` (xcap-based,
  // used by the screen-capture path). These come from Tauri's window
  // module and align with `windowing.position`.
  monitors: {
    primary(): Promise<MonitorInfo | null>;
    available(): Promise<MonitorInfo[]>;
  };
  // Structured logging routed to the Rust backend so lines reach the dev
  // terminal and the persisted client.log (WARN/ERROR only). Callers use
  // getLogger(scope) from lib/util/log.ts, not this directly.
  logging: {
    /** Fire-and-forget. `message` is already fully formatted; `scope` is passed
     *  separately so the backend can render it as the module column. */
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
