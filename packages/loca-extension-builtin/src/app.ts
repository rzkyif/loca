// Launch apps and open files on the user's computer. These run on the machine
// where Core runs, so the apps and files are the user's own when Core and the
// Client share a device. Like `open.ts`, the launcher is picked from
// `Deno.build.os`; the command-selection helpers take the OS as a parameter so
// tests can assert every platform's command without spawning anything.
//
// Windows goes through `cmd /c start`, which re-parses its command line, so any
// value handed to it (app name or file path) is rejected when it contains a
// shell metacharacter that could chain a second command. macOS `open` and
// Linux `gtk-launch`/`xdg-open` receive arguments directly with no shell, so
// they need no such guard.

import { isAbsolute } from "node:path";
import type { ToolContext } from "./types.ts";

export interface Spawn {
  bin: string;
  args: string[];
}

const UNSAFE_CMD = /[&|<>^"%\r\n]/;

function assertSafeForCmd(value: string, label: string): void {
  if (UNSAFE_CMD.test(value)) {
    throw new Error(`${label} contains characters that are not allowed`);
  }
}

// Candidate launchers for an app, tried in order until one exits 0. Linux has
// two: gtk-launch (by .desktop id) with xdg-open as the fallback.
export function pickAppCmd(os: typeof Deno.build.os, appName: string): Spawn[] {
  switch (os) {
    case "darwin":
      return [{ bin: "open", args: ["-a", appName] }];
    case "windows":
      assertSafeForCmd(appName, "app name");
      // The empty "" is `start`'s window-title argument; without it a quoted
      // app name would be consumed as the title instead of the program.
      return [{ bin: "cmd", args: ["/c", "start", "", appName] }];
    default:
      return [
        { bin: "gtk-launch", args: [appName] },
        { bin: "xdg-open", args: [appName] },
      ];
  }
}

// Candidate launchers for a file. A named app is honored on macOS/Windows; on
// Linux opening in a specific app isn't expressible with a single declarable
// binary, so it falls back to the default handler (xdg-open).
export function pickFileCmd(os: typeof Deno.build.os, filePath: string, appName?: string): Spawn[] {
  switch (os) {
    case "darwin":
      return [{ bin: "open", args: appName ? ["-a", appName, filePath] : [filePath] }];
    case "windows":
      assertSafeForCmd(filePath, "file path");
      if (appName) assertSafeForCmd(appName, "app name");
      return [
        {
          bin: "cmd",
          args: appName ? ["/c", "start", "", appName, filePath] : ["/c", "start", "", filePath],
        },
      ];
    default:
      return [{ bin: "xdg-open", args: [filePath] }];
  }
}

// True when a named app was requested but the host can't honor it (Linux), so
// the default handler is used instead.
export function appNameIgnored(os: typeof Deno.build.os, appName?: string): boolean {
  return os !== "darwin" && os !== "windows" && !!appName;
}

async function launch(candidates: Spawn[], signal: AbortSignal): Promise<void> {
  let lastErr = "";
  for (const c of candidates) {
    try {
      const proc = new Deno.Command(c.bin, {
        args: c.args,
        stdout: "null",
        stderr: "piped",
        signal,
      }).spawn();
      const { code, stderr } = await proc.output();
      if (code === 0) return;
      lastErr = new TextDecoder().decode(stderr).trim() || `${c.bin} exited ${code}`;
    } catch (err) {
      // A missing launcher binary (e.g. no gtk-launch) falls through to the
      // next candidate; the last error is reported if none succeed.
      lastErr = err instanceof Error ? err.message : String(err);
    }
  }
  throw new Error(lastErr || "no launcher available");
}

export async function openApp(
  args: { apps?: string[] },
  ctx: ToolContext,
): Promise<{ opened: string[]; failed: Array<{ app: string; error: string }> }> {
  const apps = Array.isArray(args?.apps)
    ? args.apps.map((a) => (typeof a === "string" ? a.trim() : "")).filter((a) => a.length > 0)
    : [];
  if (apps.length === 0) throw new Error("apps is required");

  const opened: string[] = [];
  const failed: Array<{ app: string; error: string }> = [];
  for (let i = 0; i < apps.length; i++) {
    const app = apps[i];
    ctx.setProgress(apps.length === 1 ? 0.5 : i / apps.length, "Launching", app);
    try {
      await launch(pickAppCmd(Deno.build.os, app), ctx.signal);
      opened.push(app);
    } catch (err) {
      failed.push({ app, error: err instanceof Error ? err.message : String(err) });
    }
  }
  ctx.setProgress(
    1,
    "Done",
    `${opened.length} opened${failed.length ? `, ${failed.length} failed` : ""}`,
  );

  if (opened.length === 0) {
    throw new Error(
      `could not open any app: ${failed.map((f) => `${f.app} (${f.error})`).join("; ")}`,
    );
  }
  return { opened, failed };
}

export async function openFile(
  args: { path?: string; appName?: string },
  ctx: ToolContext,
): Promise<{ opened: string; app?: string; usedDefaultApp: boolean }> {
  const filePath = typeof args?.path === "string" ? args.path.trim() : "";
  if (!filePath) throw new Error("path is required");
  if (!isAbsolute(filePath)) throw new Error("path must be an absolute path");
  const appName = typeof args?.appName === "string" ? args.appName.trim() : "";

  ctx.setProgress(0.3, "Checking file", filePath);
  // Stat exercises the read permission, so in ask mode the user's per-file
  // permission prompt is what gates the open.
  try {
    Deno.statSync(filePath);
  } catch (err) {
    if (err instanceof Deno.errors.NotFound) throw new Error(`file not found: ${filePath}`);
    throw err;
  }

  const ignored = appNameIgnored(Deno.build.os, appName || undefined);
  ctx.setProgress(0.6, "Opening", filePath);
  await launch(pickFileCmd(Deno.build.os, filePath, appName || undefined), ctx.signal);
  ctx.setProgress(1, "Opened", filePath);

  const usedDefaultApp = appName === "" || ignored;
  return { opened: filePath, app: usedDefaultApp ? undefined : appName, usedDefaultApp };
}
