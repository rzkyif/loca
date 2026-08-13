// Windows console suppression for sidecars, via `kernel32.dll` / `user32.dll` FFI.
//
// The core spawns console-subsystem sidecars (llama-server, loca-core-speech)
// with `Deno.Command`, which exposes NO `CREATE_NO_WINDOW` option. On Windows a
// console-subsystem child with no console to inherit ALLOCATES ITS OWN, and that
// console gets a VISIBLE window - the terminal windows users saw pop up.
//
// The fix is to make sure the CORE has a console with a HIDDEN window before it
// spawns any sidecar: children then inherit that hidden console instead of
// allocating their own visible one.
//   - On-demand launch (client `spawn_detached`) already uses CREATE_NO_WINDOW,
//     so the core has a windowless console and there's nothing to hide.
//   - Task Scheduler / launched-with-no-console launches give the core NO
//     console at all, so `GetConsoleWindow` is null; we `AllocConsole` one and
//     hide its window. That is the case this module exists for.
//
// Best-effort and idempotent: any failure is logged and swallowed. Windows-only;
// a no-op everywhere else. The core already uses FFI for `@db/sqlite` and the
// jobctl Job Object, so no new bundling or compile flags.

import { getLogger } from "../shared/log.ts";
import { errMessage } from "@loca/shared";

const log = getLogger("console");

const KERNEL32_SYMBOLS = {
  GetConsoleWindow: { parameters: [], result: "pointer" },
  AllocConsole: { parameters: [], result: "i32" },
  GetLastError: { parameters: [], result: "u32" },
} as const;

const USER32_SYMBOLS = {
  ShowWindow: { parameters: ["pointer", "i32"], result: "i32" },
} as const;

const SW_HIDE = 0;

let done = false;

/** Ensure the core process has a console whose window is hidden, so
 *  console-subsystem sidecars inherit it rather than popping their own visible
 *  console. Call once at boot BEFORE spawning any sidecar. Windows-only; a
 *  no-op (returning immediately) on every other OS and on repeat calls. */
export function ensureHiddenConsole(): void {
  if (Deno.build.os !== "windows" || done) return;
  done = true;
  let kernel32: Deno.DynamicLibrary<typeof KERNEL32_SYMBOLS> | null = null;
  let user32: Deno.DynamicLibrary<typeof USER32_SYMBOLS> | null = null;
  try {
    kernel32 = Deno.dlopen("kernel32.dll", KERNEL32_SYMBOLS);
    user32 = Deno.dlopen("user32.dll", USER32_SYMBOLS);

    // Only ever hide a console WE allocate. A pre-existing console window means
    // one of: a CREATE_NO_WINDOW launch (windowless already - GetConsoleWindow
    // returns null there, so we don't reach this), an install-time Start-Process
    // -Hidden launch (already hidden), or - critically - `deno task dev` run from
    // a terminal, where the window is the DEVELOPER'S terminal. Never SW_HIDE
    // that. We act only in the "no console at all" case (Task Scheduler /
    // detached launch), where a fresh AllocConsole gives sidecars a console to
    // inherit and its window is ours to hide.
    if (kernel32.symbols.GetConsoleWindow()) {
      log.debug("console already present; leaving it untouched");
      return;
    }
    if (kernel32.symbols.AllocConsole() === 0) {
      // Already has a (windowless) console, e.g. CREATE_NO_WINDOW: sidecars
      // inherit it and stay windowless, so there is nothing to do.
      log.debug(`AllocConsole skipped (err=${kernel32.symbols.GetLastError()})`);
      return;
    }
    const hwnd = kernel32.symbols.GetConsoleWindow();
    if (hwnd) {
      user32.symbols.ShowWindow(hwnd, SW_HIDE);
      log.debug("hid the console we allocated; sidecars will inherit it");
    }
  } catch (err) {
    log.warn(`console suppression FFI failed: ${errMessage(err)}`);
  } finally {
    kernel32?.close();
    user32?.close();
  }
}
