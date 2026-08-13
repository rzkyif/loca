// Tests for the app/file openers. The pure command builders are asserted across
// every OS; the handlers' validation branches are checked without spawning
// (which would launch real apps).

import { assertEquals, assertRejects, assertThrows } from "@std/assert";
import { appNameIgnored, openApp, openFile, pickAppCmd, pickFileCmd } from "./app.ts";
import type { ToolContext } from "./types.ts";

function emptyCtx(): ToolContext {
  return {
    setProgress() {},
    askUser: () => Promise.resolve([]),
    log() {},
    display: { markdown() {}, image() {}, table() {}, diff() {} },
    memories: {
      list: () => Promise.resolve([]),
      get: () => Promise.reject(new Error("not scripted")),
      getFile: () => Promise.reject(new Error("not scripted")),
      write: () => Promise.reject(new Error("not scripted")),
      edit: () => Promise.reject(new Error("not scripted")),
    },
    db: {
      query: () => Promise.reject(new Error("not scripted")),
      execute: () => Promise.reject(new Error("not scripted")),
    },
    llm: { complete: () => Promise.reject(new Error("not scripted")) },
    tts: { speak: () => Promise.reject(new Error("not scripted")) },
    stt: { transcribe: () => Promise.reject(new Error("not scripted")) },
    schedulePrompt: () => Promise.reject(new Error("not scripted")),
    signal: new AbortController().signal,
    getChatContext: () => ({ userMessage: "", sessionId: null }),
  };
}

Deno.test("pickAppCmd: per-OS launcher", () => {
  assertEquals(pickAppCmd("darwin", "Calculator"), [{ bin: "open", args: ["-a", "Calculator"] }]);
  assertEquals(pickAppCmd("windows", "Calculator"), [
    { bin: "cmd", args: ["/c", "start", "", "Calculator"] },
  ]);
  assertEquals(pickAppCmd("linux", "calculator"), [
    { bin: "gtk-launch", args: ["calculator"] },
    { bin: "xdg-open", args: ["calculator"] },
  ]);
});

Deno.test("pickAppCmd: rejects shell metacharacters on Windows", () => {
  assertThrows(() => pickAppCmd("windows", "a & calc"), Error, "not allowed");
  // Other platforms don't go through cmd, so metacharacters are fine.
  pickAppCmd("darwin", "a & b");
  pickAppCmd("linux", "a & b");
});

Deno.test("pickFileCmd: default app", () => {
  assertEquals(pickFileCmd("darwin", "/a/b.pdf"), [{ bin: "open", args: ["/a/b.pdf"] }]);
  assertEquals(pickFileCmd("windows", "C:\\a\\b.pdf"), [
    { bin: "cmd", args: ["/c", "start", "", "C:\\a\\b.pdf"] },
  ]);
  assertEquals(pickFileCmd("linux", "/a/b.pdf"), [{ bin: "xdg-open", args: ["/a/b.pdf"] }]);
});

Deno.test("pickFileCmd: named app honored on macOS/Windows, ignored on Linux", () => {
  assertEquals(pickFileCmd("darwin", "/a/b.pdf", "Preview"), [
    { bin: "open", args: ["-a", "Preview", "/a/b.pdf"] },
  ]);
  assertEquals(pickFileCmd("windows", "C:\\b.pdf", "Acrobat"), [
    { bin: "cmd", args: ["/c", "start", "", "Acrobat", "C:\\b.pdf"] },
  ]);
  // Linux falls back to the default handler regardless of appName.
  assertEquals(pickFileCmd("linux", "/a/b.pdf", "gedit"), [
    { bin: "xdg-open", args: ["/a/b.pdf"] },
  ]);
  assertEquals(appNameIgnored("linux", "gedit"), true);
  assertEquals(appNameIgnored("darwin", "Preview"), false);
  assertEquals(appNameIgnored("linux", undefined), false);
});

Deno.test("pickFileCmd: rejects shell metacharacters on Windows", () => {
  assertThrows(() => pickFileCmd("windows", "C:\\a & b.pdf"), Error, "not allowed");
  assertThrows(() => pickFileCmd("windows", "C:\\b.pdf", "a | b"), Error, "not allowed");
});

Deno.test("openApp: rejects missing/empty apps", async () => {
  await assertRejects(() => openApp({}, emptyCtx()), Error, "apps is required");
  await assertRejects(() => openApp({ apps: [] }, emptyCtx()), Error, "apps is required");
  await assertRejects(() => openApp({ apps: ["   "] }, emptyCtx()), Error, "apps is required");
});

Deno.test("openFile: rejects missing or relative path", async () => {
  await assertRejects(() => openFile({}, emptyCtx()), Error, "path is required");
  await assertRejects(() => openFile({ path: "relative/x.pdf" }, emptyCtx()), Error, "absolute");
});
