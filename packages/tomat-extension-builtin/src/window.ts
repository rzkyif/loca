// Read and restore the positions and sizes of open app windows. This is a
// best-effort, platform-specific capability: macOS drives it through osascript
// (which needs a one-time Accessibility grant for Tomat Core), Windows through
// PowerShell + the Win32 window APIs, and Linux through wmctrl on X11. Wayland
// has no protocol to position another app's windows, so the tools declare
// `platforms: ["darwin", "windows", "linux_x11"]` in tomat.json and are never
// offered on a Wayland session; no runtime Wayland check is needed here.
//
// The command builders take the OS as a parameter and are pure, so tests assert
// every platform's command without spawning anything.

import type { ToolContext } from "./types.ts";

export interface WindowRect {
  app: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Spawn {
  bin: string;
  args: string[];
}

// One set command plus the apps it covers. Linux issues one wmctrl call per
// window (so each window's success is tracked individually); macOS and Windows
// apply every window in a single script.
export interface SetStep {
  spawn: Spawn;
  apps: string[];
}

// --- macOS (osascript) ----------------------------------------------------

function escapeAppleScript(s: string): string {
  return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

const MAC_GET_SCRIPT = [
  'set out to ""',
  'tell application "System Events"',
  "  repeat with proc in (every process whose visible is true)",
  "    set pname to name of proc",
  "    repeat with w in (every window of proc)",
  "      try",
  "        set p to position of w",
  "        set z to size of w",
  "        set out to out & pname & tab & (item 1 of p) & tab & (item 2 of p) & tab & (item 1 of z) & tab & (item 2 of z) & linefeed",
  "      end try",
  "    end repeat",
  "  end repeat",
  "end tell",
  "return out",
].join("\n");

function macSetScript(windows: WindowRect[]): string {
  const blocks = windows.map((w) =>
    [
      'tell application "System Events"',
      `  try`,
      `    tell (first process whose name is "${escapeAppleScript(w.app)}")`,
      `      set position of window 1 to {${w.x}, ${w.y}}`,
      `      set size of window 1 to {${w.width}, ${w.height}}`,
      `    end tell`,
      `  end try`,
      "end tell",
    ].join("\n"),
  );
  return blocks.join("\n");
}

// --- Windows (PowerShell) -------------------------------------------------

const WIN_TYPE = [
  'Add-Type @"',
  "using System;",
  "using System.Runtime.InteropServices;",
  "public class WinApi {",
  '  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);',
  '  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int t, uint f);',
  "  public struct RECT { public int Left, Top, Right, Bottom; }",
  "}",
  '"@',
].join("\n");

const WIN_GET_SCRIPT = [
  WIN_TYPE,
  "Get-Process | Where-Object { $_.MainWindowHandle -ne 0 } | ForEach-Object {",
  "  $r = New-Object WinApi+RECT",
  "  [void][WinApi]::GetWindowRect($_.MainWindowHandle, [ref]$r)",
  '  "$($_.ProcessName)`t$($r.Left)`t$($r.Top)`t$($($r.Right) - $($r.Left))`t$($($r.Bottom) - $($r.Top))"',
  "}",
].join("\n");

function escapePowerShell(s: string): string {
  return s.replace(/'/g, "''");
}

function winSetScript(windows: WindowRect[]): string {
  const targets = windows
    .map((w) => `@{n='${escapePowerShell(w.app)}';x=${w.x};y=${w.y};w=${w.width};h=${w.height}}`)
    .join(",");
  return [
    WIN_TYPE,
    `$targets = @(${targets})`,
    "foreach ($t in $targets) {",
    "  $p = Get-Process -Name $t.n -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1",
    "  if ($p) { [void][WinApi]::SetWindowPos($p.MainWindowHandle, [IntPtr]::Zero, $t.x, $t.y, $t.w, $t.h, 0x0040) }",
    "}",
  ].join("\n");
}

// --- command selection ----------------------------------------------------

export function pickGetCmd(os: typeof Deno.build.os): Spawn {
  switch (os) {
    case "darwin":
      return { bin: "osascript", args: ["-e", MAC_GET_SCRIPT] };
    case "windows":
      return { bin: "powershell", args: ["-NoProfile", "-Command", WIN_GET_SCRIPT] };
    default:
      return { bin: "wmctrl", args: ["-lG"] };
  }
}

export function pickSetSteps(os: typeof Deno.build.os, windows: WindowRect[]): SetStep[] {
  const apps = windows.map((w) => w.app);
  switch (os) {
    case "darwin":
      return [{ spawn: { bin: "osascript", args: ["-e", macSetScript(windows)] }, apps }];
    case "windows":
      return [
        {
          spawn: { bin: "powershell", args: ["-NoProfile", "-Command", winSetScript(windows)] },
          apps,
        },
      ];
    default:
      // wmctrl matches a window by a substring of its title; -e takes
      // gravity,x,y,width,height (gravity 0 = use the window's current gravity).
      return windows.map((w) => ({
        spawn: {
          bin: "wmctrl",
          args: ["-r", w.app, "-e", `0,${w.x},${w.y},${w.width},${w.height}`],
        },
        apps: [w.app],
      }));
  }
}

// Parse a get command's stdout into window rects. macOS/Windows emit
// tab-separated `app\tx\ty\tw\th`; wmctrl -lG emits
// `id desktop x y w h host title`.
export function parseWindows(os: typeof Deno.build.os, stdout: string): WindowRect[] {
  const out: WindowRect[] = [];
  for (const line of stdout.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    if (os === "darwin" || os === "windows") {
      const parts = trimmed.split("\t");
      if (parts.length < 5) continue;
      const [app, x, y, w, h] = parts;
      const rect = toRect(app, x, y, w, h);
      if (rect) out.push(rect);
    } else {
      const parts = trimmed.split(/\s+/);
      if (parts.length < 8) continue;
      const app = parts.slice(7).join(" ");
      const rect = toRect(app, parts[2], parts[3], parts[4], parts[5]);
      if (rect) out.push(rect);
    }
  }
  return out;
}

function toRect(app: string, x: string, y: string, w: string, h: string): WindowRect | null {
  const nx = Number(x),
    ny = Number(y),
    nw = Number(w),
    nh = Number(h);
  if (![nx, ny, nw, nh].every(Number.isFinite)) return null;
  return { app, x: nx, y: ny, width: nw, height: nh };
}

// --- handlers -------------------------------------------------------------

async function runCapture(spawn: Spawn, signal: AbortSignal): Promise<string> {
  const proc = new Deno.Command(spawn.bin, {
    args: spawn.args,
    stdout: "piped",
    stderr: "piped",
    signal,
  }).spawn();
  const { code, stdout, stderr } = await proc.output();
  if (code !== 0) {
    const msg = new TextDecoder().decode(stderr).trim();
    throw new Error(`${spawn.bin} exited ${code}: ${msg || "no stderr"}`);
  }
  return new TextDecoder().decode(stdout);
}

export async function getWindowLayout(
  _args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ windows: WindowRect[] }> {
  ctx.setProgress(0.5, "Reading windows");
  const stdout = await runCapture(pickGetCmd(Deno.build.os), ctx.signal);
  const windows = parseWindows(Deno.build.os, stdout);
  ctx.setProgress(1, "Done", `${windows.length} windows`);
  return { windows };
}

function normalizeWindows(input: unknown): WindowRect[] {
  if (!Array.isArray(input)) return [];
  const out: WindowRect[] = [];
  for (const w of input) {
    if (!w || typeof w !== "object") continue;
    const r = w as Record<string, unknown>;
    const app = typeof r.app === "string" ? r.app.trim() : "";
    const x = Number(r.x),
      y = Number(r.y),
      width = Number(r.width),
      height = Number(r.height);
    if (!app || ![x, y, width, height].every(Number.isFinite)) continue;
    out.push({ app, x, y, width, height });
  }
  return out;
}

export async function setWindowLayout(
  args: { windows?: unknown },
  ctx: ToolContext,
): Promise<{ applied: string[]; failed: Array<{ app: string; error: string }> }> {
  const windows = normalizeWindows(args?.windows);
  if (windows.length === 0) throw new Error("windows is required");

  const steps = pickSetSteps(Deno.build.os, windows);
  const applied: string[] = [];
  const failed: Array<{ app: string; error: string }> = [];
  for (let i = 0; i < steps.length; i++) {
    const step = steps[i];
    ctx.setProgress((i + 1) / (steps.length + 1), "Placing windows");
    try {
      await runCapture(step.spawn, ctx.signal);
      applied.push(...step.apps);
    } catch (err) {
      const error = err instanceof Error ? err.message : String(err);
      for (const app of step.apps) failed.push({ app, error });
    }
  }
  ctx.setProgress(
    1,
    "Done",
    `${applied.length} placed${failed.length ? `, ${failed.length} failed` : ""}`,
  );

  if (applied.length === 0) {
    throw new Error(
      `could not place any window: ${failed.map((f) => `${f.app} (${f.error})`).join("; ")}`,
    );
  }
  return { applied, failed };
}
