// Tests for the window-layout command builders and parsing. The actual
// subprocess runs (osascript/powershell/wmctrl) are not exercised; only the
// pure command selection, output parsing, and handler validation are.

import { assertEquals, assertRejects } from "@std/assert";
import { parseWindows, pickGetCmd, pickSetSteps, setWindowLayout } from "./window.ts";
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

Deno.test("pickGetCmd: per-OS reader", () => {
  assertEquals(pickGetCmd("darwin").bin, "osascript");
  assertEquals(pickGetCmd("windows").bin, "powershell");
  assertEquals(pickGetCmd("linux"), { bin: "wmctrl", args: ["-lG"] });
});

Deno.test("pickSetSteps: macOS/Windows is one step, Linux is one per window", () => {
  const windows = [
    { app: "Preview", x: 0, y: 0, width: 800, height: 600 },
    { app: "Safari", x: 800, y: 0, width: 800, height: 600 },
  ];
  assertEquals(pickSetSteps("darwin", windows).length, 1);
  assertEquals(pickSetSteps("darwin", windows)[0].apps, ["Preview", "Safari"]);
  assertEquals(pickSetSteps("windows", windows).length, 1);

  const linux = pickSetSteps("linux", windows);
  assertEquals(linux.length, 2);
  assertEquals(linux[0].spawn, { bin: "wmctrl", args: ["-r", "Preview", "-e", "0,0,0,800,600"] });
  assertEquals(linux[1].apps, ["Safari"]);
});

Deno.test("parseWindows: tab-separated for macOS/Windows", () => {
  const out = parseWindows("darwin", "Preview\t10\t20\t800\t600\nSafari\t0\t0\t1024\t768\n");
  assertEquals(out, [
    { app: "Preview", x: 10, y: 20, width: 800, height: 600 },
    { app: "Safari", x: 0, y: 0, width: 1024, height: 768 },
  ]);
});

Deno.test("parseWindows: wmctrl -lG columns for Linux", () => {
  // id desktop x y w h host title
  const line = "0x0240 0 12 34 800 600 myhost My Document - Editor\n";
  assertEquals(parseWindows("linux", line), [
    { app: "My Document - Editor", x: 12, y: 34, width: 800, height: 600 },
  ]);
});

Deno.test("setWindowLayout: rejects empty windows", async () => {
  await assertRejects(() => setWindowLayout({}, emptyCtx()), Error, "windows is required");
  await assertRejects(
    () => setWindowLayout({ windows: [] }, emptyCtx()),
    Error,
    "windows is required",
  );
  await assertRejects(
    () => setWindowLayout({ windows: [{ app: "", x: 0, y: 0, width: 1, height: 1 }] }, emptyCtx()),
    Error,
    "windows is required",
  );
});
