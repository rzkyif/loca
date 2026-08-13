// Locks in the contracts the test platform stub advertises (no-ops vs throws
// vs pure-string helpers), so a test relying on one of them has a stable base.

import { describe, expect, it } from "vitest";
import { installPlatformStub } from "./platform-stub.ts";
import { platform } from "../lib/platform/index.ts";

describe("platform stub", () => {
  installPlatformStub();
  const p = platform();

  describe("updater", () => {
    it("getVersion returns a placeholder", async () => {
      expect(await p.updater.getVersion()).toBe("0.0.0-stub");
    });
    it("check returns null (no auto-update under tests)", async () => {
      expect(await p.updater.check()).toBeNull();
    });
    it("relaunch throws: no process to relaunch", async () => {
      await expect(p.updater.relaunch()).rejects.toThrow(/relaunch/);
    });
  });

  describe("fs", () => {
    it("join concatenates segments with single slashes", async () => {
      expect(await p.fs.join("a", "b", "c")).toBe("a/b/c");
      expect(await p.fs.join("/tmp/", "/foo/", "/bar")).toBe("/tmp/foo/bar");
      expect(await p.fs.join("/abs", "rel")).toBe("/abs/rel");
    });
    it("join skips empty segments", async () => {
      expect(await p.fs.join("a", "", "b")).toBe("a/b");
    });
    it("readFile / writeFile / remove / tempDir throw", async () => {
      await expect(p.fs.readFile("x")).rejects.toThrow();
      await expect(p.fs.writeFile("x", new Uint8Array())).rejects.toThrow();
      await expect(p.fs.remove("x")).rejects.toThrow();
      await expect(p.fs.tempDir()).rejects.toThrow();
    });
  });

  describe("dialog / cursor / menu / monitors", () => {
    it("openFilePicker throws (no native dialog under tests)", async () => {
      await expect(p.dialog.openFilePicker()).rejects.toThrow();
    });
    it("cursor.getPosition throws; setClickthrough is a no-op", async () => {
      await expect(p.cursor.getPosition()).rejects.toThrow();
      await expect(p.cursor.setClickthrough(true)).resolves.toBeUndefined();
    });
    it("showContextMenu throws (no native menu)", async () => {
      await expect(p.menu.showContextMenu([{ id: "x", label: "X" }])).rejects.toThrow();
    });
    it("monitors.primary returns null and available returns []", async () => {
      expect(await p.monitors.primary()).toBeNull();
      expect(await p.monitors.available()).toEqual([]);
    });
  });
});
