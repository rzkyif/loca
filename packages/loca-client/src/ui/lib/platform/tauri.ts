// Tauri implementation of the Platform interface. The only file outside of
// itself that may import `@tauri-apps/*` (enforced by oxlint).

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  availableMonitors as tauriAvailableMonitors,
  currentMonitor as tauriCurrentMonitor,
  cursorPosition,
  getCurrentWindow,
  type Monitor as TauriMonitor,
  primaryMonitor as tauriPrimaryMonitor,
} from "@tauri-apps/api/window";
import { PhysicalPosition, PhysicalSize } from "@tauri-apps/api/dpi";
import { join as tauriJoin, tempDir as tauriTempDir } from "@tauri-apps/api/path";
import { getVersion as tauriGetVersion } from "@tauri-apps/api/app";
import { CheckMenuItem, Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  BaseDirectory,
  readFile as tauriReadFile,
  remove,
  writeFile as tauriWriteFile,
} from "@tauri-apps/plugin-fs";
import {
  disable as autostartDisable,
  enable as autostartEnable,
  isEnabled as autostartIsEnabled,
} from "@tauri-apps/plugin-autostart";
import { check as tauriUpdaterCheck, type Update } from "@tauri-apps/plugin-updater";
import { relaunch as tauriRelaunch } from "@tauri-apps/plugin-process";
import {
  type ContextMenuItem,
  type MonitorInfo,
  type Platform,
  setPlatform,
  type UpdateHandle,
} from "./index";
import { net } from "./shared";
import { getLogger } from "$lib/util/log";

const log = getLogger("platform");

export function installTauriPlatform(): void {
  setPlatform(impl);
}

const impl: Platform = {
  // Pinned HTTP + WebSocket to a paired core; shared with the mobile impl
  // since both call the same Rust commands (see lib/platform/shared.ts).
  net,
  windowing: {
    show: () => invoke("show_main_window"),
    hide: () => invoke("hide_main_window"),
    toggle: () => invoke("toggle_main_window"),
    requestHide: () => invoke("request_hide_main_window"),
    async position(args) {
      await invoke("position_window", args);
    },
    async isVisible() {
      try {
        return (await getCurrentWindow().isVisible()) ?? false;
      } catch {
        return false;
      }
    },
    async subscribeVisibility(cb) {
      const unlisten = await listen<boolean>("window-visibility", (e) => cb(!!e.payload));
      return () => unlisten();
    },
    async outerSize() {
      const s = await getCurrentWindow().outerSize();
      return { width: s.width, height: s.height };
    },
    async outerPosition() {
      const p = await getCurrentWindow().outerPosition();
      return { x: p.x, y: p.y };
    },
    async setOuterSize({ width, height }) {
      await getCurrentWindow().setSize(new PhysicalSize(width, height));
    },
    async setOuterPosition({ x, y }) {
      await getCurrentWindow().setPosition(new PhysicalPosition(x, y));
    },
    async currentMonitor() {
      const m = await tauriCurrentMonitor();
      if (!m) return null;
      const primary = await tauriPrimaryMonitor();
      return toMonitorInfo(m, primary?.name === m.name);
    },
    async subscribeHideRequested(cb) {
      const unlisten = await listen("window-hide-requested", () => cb());
      return () => unlisten();
    },
    async subscribeMonitorChanged(cb) {
      const unlisten = await listen("monitor-changed", () => cb());
      return () => unlisten();
    },
  },
  backButton: {
    // Desktop has no hardware back button: the subscription never fires and the
    // registry's back() is only reached via this stream, so exit is unreachable.
    subscribe: async () => () => {},
    exit: () => Promise.resolve(),
    canExit: () => false,
  },
  autostart: {
    isEnabled: () => autostartIsEnabled(),
    async setEnabled(enabled) {
      if (enabled) await autostartEnable();
      else await autostartDisable();
    },
    wasAutostarted: () => invoke("was_autostarted"),
  },
  capture: {
    monitors: () => invoke("list_capture_monitors"),
    captureMonitor: (monitorId) => invoke("capture_monitor", { monitorId }),
    setRegionTarget: (monitorId) => invoke("set_region_capture_target", { monitorId }),
    getRegionTarget: () => invoke("get_region_capture_target"),
    showRegionOverlay: () => invoke("show_region_capture_overlay"),
    hideRegionOverlay: () => invoke("hide_region_capture_overlay"),
    async subscribeRegionResult(cb) {
      const unResult: UnlistenFn = await listen<string>("region-capture-result", (e) =>
        cb(e.payload || null),
      );
      const unCancel: UnlistenFn = await listen("region-capture-cancelled", () => cb(null));
      return () => {
        unResult();
        unCancel();
      };
    },
  },
  audio: {
    getSystemVolume: () => invoke("get_system_volume"),
    setSystemVolume: (percent) => invoke("set_system_volume", { percent }),
    restoreSystemVolume: () => invoke("restore_system_volume"),
  },
  fonts: {
    list: () => invoke("list_system_fonts"),
  },
  process: {
    selfMetrics: () => invoke("get_self_metrics"),
  },
  clientStorage: {
    tree: () => invoke("get_client_storage"),
    truncateActiveLog: () => invoke("truncate_client_log"),
  },
  shortcuts: {
    setBinding(accelerator) {
      return invoke("set_global_shortcut", { accelerator });
    },
    validate(accelerator) {
      return invoke("validate_shortcut", { accelerator });
    },
    setPttConfig(pushToTalk, holdMs) {
      return invoke("set_ptt_config", { pushToTalk, holdMs });
    },
    async subscribeEvents({ onPressed, onTap, onHoldStart, onHoldEnd }) {
      const unlisteners: UnlistenFn[] = [await listen("shortcut-pressed", () => onPressed())];
      if (onTap) unlisteners.push(await listen("shortcut-tap", () => onTap()));
      if (onHoldStart) unlisteners.push(await listen("shortcut-hold-start", () => onHoldStart()));
      if (onHoldEnd) unlisteners.push(await listen("shortcut-hold-end", () => onHoldEnd()));
      return () => {
        for (const un of unlisteners) un();
      };
    },
    setInputBindings(bindings) {
      return invoke("set_input_shortcuts", { bindings });
    },
    async subscribeInputEvents({ onAttachFile, onCaptureScreen, onCaptureRegion }) {
      const unlisteners: UnlistenFn[] = [];
      if (onAttachFile) {
        unlisteners.push(await listen("input-shortcut-attach-file", () => onAttachFile()));
      }
      if (onCaptureScreen) {
        unlisteners.push(await listen("input-shortcut-capture-screen", () => onCaptureScreen()));
      }
      if (onCaptureRegion) {
        unlisteners.push(await listen("input-shortcut-capture-region", () => onCaptureRegion()));
      }
      return () => {
        for (const u of unlisteners) u();
      };
    },
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
    readAdminToken: () => invoke("read_admin_token"),
    readLocalCoreBootError: () => invoke("read_local_core_boot_error"),
    isLocalCoreInstalled: () => invoke("local_core_installed"),
    isLocalCoreRunning: () => invoke("local_core_running"),
    startLocalCore: () => invoke("start_local_core"),
    stopLocalCore: () => invoke("stop_local_core"),
    localCoreBaseUrl: () => invoke("local_core_base_url"),
    localSidecarPorts: () => invoke("local_sidecar_ports"),
    launchPrefill: () => invoke("read_launch_prefill"),
  },
  fileConvert: {
    toMarkdownFromPath(absPath) {
      return invoke<string>("convert_file_to_markdown", { filePath: absPath });
    },
    async toMarkdown(file) {
      // Save the file to a temp location via Tauri, then convert. The
      // current `convert_file_to_markdown` Tauri command takes a path; the
      // Svelte UI receives a File from a drag/drop or picker. To bridge,
      // we write the file to a temp path first, then delete the temp file
      // in a finally block so converting many files in a session doesn't
      // accumulate orphans in the OS temp dir.
      const { absPath, relPath } = await fileToTempPath(file);
      try {
        return await invoke<string>("convert_file_to_markdown", {
          filePath: absPath,
        });
      } finally {
        try {
          await remove(relPath, { baseDir: BaseDirectory.Temp });
        } catch (e) {
          log.warn("temp file cleanup failed:", e);
        }
      }
    },
  },
  resolvePath: (path) => invoke("resolve_path", { path }),
  openExternal: (url) => openUrl(url),
  revealPath: (absPath) => revealItemInDir(absPath),
  updater: {
    getVersion: () => tauriGetVersion(),
    async check() {
      const update = await tauriUpdaterCheck();
      if (!update) return null;
      return wrapUpdate(update);
    },
    canSelfInstall: () => invoke("can_self_install"),
    relaunch: () => tauriRelaunch(),
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
      const picked = await openDialog({
        multiple: opts?.multiple ?? false,
        filters: opts?.filters,
      });
      if (picked === null) return [];
      return Array.isArray(picked) ? picked : [picked];
    },
  },
  cursor: {
    getPosition: () => cursorPosition(),
    setClickthrough: (enabled) => getCurrentWindow().setIgnoreCursorEvents(enabled),
  },
  menu: {
    async showContextMenu(items: ContextMenuItem[]) {
      // Tauri's Menu fires per-item actions; we resolve the returned
      // promise with the chosen id (or null on dismiss). Separators
      // pass through as `{ item: "Separator" }` in the underlying API.
      let resolved: ((id: string | null) => void) | null = null;
      const result = new Promise<string | null>((resolve) => {
        resolved = resolve;
      });
      // Build concrete item instances so check items (CheckMenuItem) and
      // separators (PredefinedMenuItem) are unambiguous to the native layer.
      const menuItems = await Promise.all(
        items.map((item) => {
          if ("separator" in item) {
            return PredefinedMenuItem.new({ item: "Separator" });
          }
          const opts = {
            id: item.id,
            text: item.label,
            enabled: item.enabled ?? true,
            action: () => {
              if (resolved) resolved(item.id);
            },
          };
          return item.checked === undefined
            ? MenuItem.new(opts)
            : CheckMenuItem.new({ ...opts, checked: item.checked });
        }),
      );
      const menu = await Menu.new({ items: menuItems });
      await menu.popup();
      // popup() resolves when the menu CLOSES (click OR dismiss). A selected
      // item's action arrives slightly later over an async IPC channel, so we
      // can't conclude "dismissed" immediately: a 0ms timer reliably beats the
      // action message and would report a real selection as a dismissal
      // (silently no-oping destructive actions). Wait one comfortable IPC
      // round-trip for the action to land first. On selection the action
      // resolves `result` and this timer is a no-op; on a true dismiss we
      // resolve null after the grace window (imperceptible, since a dismiss does
      // nothing anyway). The Tauri JS API exposes no menu-closed event, so this
      // bounded grace is the available signal.
      const DISMISS_GRACE_MS = 150;
      const dismissTimer = setTimeout(() => {
        if (resolved) resolved(null);
      }, DISMISS_GRACE_MS);
      try {
        return await result;
      } finally {
        clearTimeout(dismissTimer);
      }
    },
  },
  tray: {
    setCoreState: (installed, running) => invoke("set_tray_core_state", { installed, running }),
    async subscribeCoreAction(cb) {
      const offs = await Promise.all([
        listen("tray-core-start", () => cb("start")),
        listen("tray-core-stop", () => cb("stop")),
      ]);
      return () => offs.forEach((off) => off());
    },
  },
  monitors: {
    async primary() {
      const m = await tauriPrimaryMonitor();
      return m ? toMonitorInfo(m, true) : null;
    },
    async available() {
      const list = await tauriAvailableMonitors();
      const primary = await tauriPrimaryMonitor();
      return list.map((m) => toMonitorInfo(m, primary?.name === m.name));
    },
  },
  logging: {
    log(level, scope, message) {
      // Fire-and-forget into the Rust fern sinks; a failed IPC (e.g. during
      // teardown) must never throw into the caller.
      void invoke("client_log", { level, scope, message }).catch(() => {});
    },
  },
};

function wrapUpdate(update: Update): UpdateHandle {
  return {
    version: update.version,
    notes: update.body ?? undefined,
    async downloadAndInstall(onProgress) {
      await update.downloadAndInstall((event) => {
        if (!onProgress) return;
        if (event.event === "Started") {
          onProgress({ kind: "Started", total: event.data.contentLength });
        } else if (event.event === "Progress") {
          onProgress({ kind: "Progress", chunk: event.data.chunkLength });
        } else if (event.event === "Finished") {
          onProgress({ kind: "Finished" });
        }
      });
    },
    close: () => update.close(),
  };
}

function toMonitorInfo(m: TauriMonitor, isPrimary: boolean): MonitorInfo {
  return {
    id: m.name ?? `${m.position.x},${m.position.y}`,
    name: m.name ?? "unnamed",
    isPrimary,
    x: m.position.x,
    y: m.position.y,
    width: m.size.width,
    height: m.size.height,
  };
}

// Tauri command for file-to-markdown takes a path. The Svelte File object
// doesn't expose its on-disk path (it's a Blob). Solution: write the bytes
// to a Tauri-managed temp file via tauri-plugin-fs, then pass that path.
// Returns both forms so the caller can both invoke the command (needs the
// absolute path, since the convert command runs outside the FS-plugin sandbox)
// and clean up afterwards (the remove() API takes the relative path + base).
async function fileToTempPath(file: File): Promise<{ absPath: string; relPath: string }> {
  const safe = file.name.replace(/[^a-zA-Z0-9._-]/g, "_");
  const relPath = `loca-tmp-${Date.now()}-${safe}`;
  const bytes = new Uint8Array(await file.arrayBuffer());
  await tauriWriteFile(relPath, bytes, { baseDir: BaseDirectory.Temp });
  const absPath = await tauriJoin(await tauriTempDir(), relPath);
  return { absPath, relPath };
}
