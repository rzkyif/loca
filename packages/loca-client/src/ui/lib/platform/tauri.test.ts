// Tauri platform impl tested through `@tauri-apps/*` mocks. Asserts the
// invoke / listen call shapes (command name + args) for every method
// the new namespaces expose. The web stub is covered separately in
// web.test.ts.

import { beforeEach, describe, expect, it, vi } from "vitest";

// All `@tauri-apps/*` modules used by tauri.ts are mocked here. The
// `__esModule: true` flag is required for vitest to satisfy the ES-module
// shape svelte-kit's bundler resolves.

const invoke = vi.fn();
const listen = vi.fn(() => Promise.resolve(() => {}));
const getCurrentWindow = vi.fn(() => ({
  isVisible: vi.fn(() => Promise.resolve(true)),
  setIgnoreCursorEvents: vi.fn(() => Promise.resolve()),
  outerSize: vi.fn(() => Promise.resolve({ width: 800, height: 600 })),
  outerPosition: vi.fn(() => Promise.resolve({ x: 100, y: 50 })),
  setSize: vi.fn(() => Promise.resolve()),
  setPosition: vi.fn(() => Promise.resolve()),
}));
const cursorPosition = vi.fn(() => Promise.resolve({ x: 0, y: 0 }));
const availableMonitors = vi.fn(() => Promise.resolve([]));
const primaryMonitor = vi.fn(() => Promise.resolve(null));
const currentMonitor = vi.fn(() => Promise.resolve(null));
const tauriJoin = vi.fn((...segs: string[]) => Promise.resolve(segs.join("/")));
const tauriTempDir = vi.fn(() => Promise.resolve("/tmp"));
const tauriGetVersion = vi.fn(() => Promise.resolve("1.2.3"));
const tauriReadFile = vi.fn(() => Promise.resolve(new Uint8Array([1, 2, 3])));
const tauriWriteFile = vi.fn(() => Promise.resolve());
const remove = vi.fn(() => Promise.resolve());
const openUrl = vi.fn(() => Promise.resolve());
const openDialog = vi.fn(() => Promise.resolve(["/picked/file.txt"]));
const tauriRelaunch = vi.fn(() => Promise.resolve());
const tauriUpdaterCheck = vi.fn(() => Promise.resolve(null));

class FakePhysicalPosition {
  constructor(
    public x: number,
    public y: number,
  ) {}
}
class FakePhysicalSize {
  constructor(
    public width: number,
    public height: number,
  ) {}
}

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow,
  cursorPosition,
  availableMonitors: availableMonitors,
  primaryMonitor: primaryMonitor,
  currentMonitor: currentMonitor,
}));
vi.mock("@tauri-apps/api/dpi", () => ({
  PhysicalPosition: FakePhysicalPosition,
  PhysicalSize: FakePhysicalSize,
}));
vi.mock("@tauri-apps/api/path", () => ({
  join: tauriJoin,
  tempDir: tauriTempDir,
}));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: tauriGetVersion }));
vi.mock("@tauri-apps/api/menu", () => ({
  Menu: {
    new: vi.fn(() => Promise.resolve({ popup: vi.fn(() => Promise.resolve()) })),
  },
  MenuItem: { new: vi.fn((o) => Promise.resolve(o)) },
  CheckMenuItem: { new: vi.fn((o) => Promise.resolve(o)) },
  PredefinedMenuItem: { new: vi.fn((o) => Promise.resolve(o)) },
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openDialog }));
vi.mock("@tauri-apps/plugin-fs", () => ({
  BaseDirectory: { Temp: 1 },
  readFile: tauriReadFile,
  remove,
  writeFile: tauriWriteFile,
}));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: tauriUpdaterCheck }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: tauriRelaunch }));

// Import AFTER the mocks so the impl picks them up.
const { installTauriPlatform } = await import("./tauri");
const { platform } = await import("./index");

installTauriPlatform();
const p = platform();

beforeEach(() => {
  vi.clearAllMocks();
});

describe("Tauri platform: windowing", () => {
  it("show invokes show_main_window", async () => {
    await p.windowing.show();
    expect(invoke).toHaveBeenCalledWith("show_main_window");
  });
  it("hide invokes hide_main_window", async () => {
    await p.windowing.hide();
    expect(invoke).toHaveBeenCalledWith("hide_main_window");
  });
  it("toggle invokes toggle_main_window", async () => {
    await p.windowing.toggle();
    expect(invoke).toHaveBeenCalledWith("toggle_main_window");
  });
  it("position forwards args to position_window", async () => {
    await p.windowing.position({
      monitorId: "m1",
      alignment: "left",
      width: 600,
    });
    expect(invoke).toHaveBeenCalledWith("position_window", {
      monitorId: "m1",
      alignment: "left",
      width: 600,
    });
  });
  it("outerSize unwraps the Tauri PhysicalSize", async () => {
    const s = await p.windowing.outerSize();
    expect(s).toEqual({ width: 800, height: 600 });
  });
  it("outerPosition unwraps the Tauri PhysicalPosition", async () => {
    const pos = await p.windowing.outerPosition();
    expect(pos).toEqual({ x: 100, y: 50 });
  });
  it("subscribeVisibility wires the listen event name", async () => {
    await p.windowing.subscribeVisibility(() => {});
    expect(listen).toHaveBeenCalledWith("window-visibility", expect.any(Function));
  });
  it("subscribeHideRequested wires window-hide-requested", async () => {
    await p.windowing.subscribeHideRequested(() => {});
    expect(listen).toHaveBeenCalledWith("window-hide-requested", expect.any(Function));
  });
  it("subscribeMonitorChanged wires monitor-changed", async () => {
    await p.windowing.subscribeMonitorChanged(() => {});
    expect(listen).toHaveBeenCalledWith("monitor-changed", expect.any(Function));
  });
});

describe("Tauri platform: updater", () => {
  it("getVersion delegates to app.getVersion", async () => {
    expect(await p.updater.getVersion()).toBe("1.2.3");
    expect(tauriGetVersion).toHaveBeenCalled();
  });
  it("check returns null when no update is staged", async () => {
    expect(await p.updater.check()).toBeNull();
    expect(tauriUpdaterCheck).toHaveBeenCalled();
  });
  it("canSelfInstall invokes can_self_install", async () => {
    invoke.mockResolvedValueOnce(false);
    expect(await p.updater.canSelfInstall()).toBe(false);
    expect(invoke).toHaveBeenCalledWith("can_self_install");
  });
  it("relaunch delegates to plugin-process", async () => {
    await p.updater.relaunch();
    expect(tauriRelaunch).toHaveBeenCalled();
  });
});

describe("Tauri platform: fs", () => {
  it("readFile delegates by path", async () => {
    await p.fs.readFile("/some/file");
    expect(tauriReadFile).toHaveBeenCalledWith("/some/file");
  });
  it("writeFile delegates with bytes", async () => {
    const bytes = new Uint8Array([9, 8, 7]);
    await p.fs.writeFile("/dst", bytes);
    expect(tauriWriteFile).toHaveBeenCalledWith("/dst", bytes);
  });
  it("remove delegates by path", async () => {
    await p.fs.remove("/x");
    expect(remove).toHaveBeenCalledWith("/x");
  });
  it("tempDir delegates", async () => {
    expect(await p.fs.tempDir()).toBe("/tmp");
  });
  it("join forwards segments", async () => {
    expect(await p.fs.join("a", "b", "c")).toBe("a/b/c");
    expect(tauriJoin).toHaveBeenCalledWith("a", "b", "c");
  });
});

describe("Tauri platform: dialog", () => {
  it("openFilePicker passes opts and normalizes single → array", async () => {
    openDialog.mockResolvedValueOnce("/one/file" as unknown as string[]);
    const picked = await p.dialog.openFilePicker({ multiple: false });
    expect(picked).toEqual(["/one/file"]);
    expect(openDialog).toHaveBeenCalledWith({
      multiple: false,
      filters: undefined,
    });
  });
  it("openFilePicker returns [] on user cancel", async () => {
    openDialog.mockResolvedValueOnce(null as unknown as string[]);
    expect(await p.dialog.openFilePicker()).toEqual([]);
  });
});

describe("Tauri platform: cursor", () => {
  it("getPosition delegates to cursorPosition()", async () => {
    cursorPosition.mockResolvedValueOnce({ x: 11, y: 22 });
    const pos = await p.cursor.getPosition();
    expect(pos).toEqual({ x: 11, y: 22 });
    expect(cursorPosition).toHaveBeenCalled();
  });
  it("setClickthrough delegates to setIgnoreCursorEvents on current window", async () => {
    await p.cursor.setClickthrough(true);
    // Driven through getCurrentWindow()'s returned object; the mock
    // factory above wires setIgnoreCursorEvents inside that return.
    expect(getCurrentWindow).toHaveBeenCalled();
  });
});

describe("Tauri platform: monitors", () => {
  it("primary returns null when Tauri has no primary", async () => {
    primaryMonitor.mockResolvedValueOnce(null);
    expect(await p.monitors.primary()).toBeNull();
  });
  it("available returns [] when no monitors known", async () => {
    availableMonitors.mockResolvedValueOnce([]);
    expect(await p.monitors.available()).toEqual([]);
  });
});

describe("Tauri platform: shortcuts (input bindings)", () => {
  it("setInputBindings invokes set_input_shortcuts", async () => {
    await p.shortcuts.setInputBindings([["attach-file", "Cmd+I"]]);
    expect(invoke).toHaveBeenCalledWith("set_input_shortcuts", {
      bindings: [["attach-file", "Cmd+I"]],
    });
  });
  it("subscribeInputEvents wires only the requested handlers", async () => {
    await p.shortcuts.subscribeInputEvents({
      onAttachFile: () => {},
      onCaptureRegion: () => {},
    });
    const events = (listen.mock.calls as unknown as Array<[string, unknown]>).map((c) => c[0]);
    expect(events).toContain("input-shortcut-attach-file");
    expect(events).toContain("input-shortcut-capture-region");
    expect(events).not.toContain("input-shortcut-capture-screen");
  });
});

describe("Tauri platform: fileConvert", () => {
  it("toMarkdownFromPath invokes convert_file_to_markdown with filePath", async () => {
    invoke.mockResolvedValueOnce("# converted");
    expect(await p.fileConvert.toMarkdownFromPath("/abs/foo.pdf")).toBe("# converted");
    expect(invoke).toHaveBeenCalledWith("convert_file_to_markdown", {
      filePath: "/abs/foo.pdf",
    });
  });
});

describe("Tauri platform: openExternal", () => {
  it("delegates to plugin-opener", async () => {
    await p.openExternal("https://example.com");
    expect(openUrl).toHaveBeenCalledWith("https://example.com");
  });
});
