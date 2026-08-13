// In-core Windows Job Object management via `kernel32.dll` FFI.
//
// On Windows, core creates a Job Object with KILL_ON_JOB_CLOSE at first use
// and assigns every freshly-spawned sidecar PID to it. Core holds the only
// handle to the job; when core exits, gracefully OR via a hard kill
// (SIGKILL / `taskkill /F`), the OS closes that handle, the job closes, and
// the kernel terminates every assigned process. This is the only way to
// guarantee orphan-free sidecar cleanup on Windows when core itself dies
// without running its shutdown path.
//
// This previously lived in a separate `loca-core-jobctl` native helper that
// core spawned and fed PIDs over a stdin pipe. It is now done in-process:
// `kernel32.dll` is a system library resolved by name, so nothing is bundled
// and no compile flags change (core already uses FFI for `@db/sqlite`).
//
// jobctl is Windows-only by necessity, not omission. On Linux and macOS the
// service supervisor already reaps orphaned sidecars when core dies hard:
// systemd kills the unit's cgroup (default KillMode=control-group) and
// launchd kills the job's process group (AbandonProcessGroup defaults to
// false). Windows Task Scheduler does neither, so on Windows core must own
// the cleanup itself. This module therefore no-ops cleanly on non-Windows;
// the only uncovered case there is core run outside its supervisor (e.g.
// `deno task dev`), which is not a production path.

import { getLogger } from "../shared/log.ts";
import { errMessage } from "@loca/shared";

const log = getLogger("jobctl");

// Win32 constants (see the original native reference for the canonical
// values it used).
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION = 9; // JOBOBJECTINFOCLASS value
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x2000;
const PROCESS_ACCESS = 0x0101; // PROCESS_SET_QUOTA | PROCESS_TERMINATE

// JOBOBJECT_EXTENDED_LIMIT_INFORMATION is 144 bytes on 64-bit Windows
// (x86_64 and aarch64 share the LLP64 layout). The only field we set is
// BasicLimitInformation.LimitFlags, a u32 at byte offset 16.
const EXT_LIMIT_INFO_SIZE = 144;
const LIMIT_FLAGS_OFFSET = 16;

const KERNEL32_SYMBOLS = {
  CreateJobObjectW: { parameters: ["pointer", "pointer"], result: "pointer" },
  SetInformationJobObject: {
    parameters: ["pointer", "i32", "pointer", "u32"],
    result: "i32",
  },
  OpenProcess: { parameters: ["u32", "i32", "u32"], result: "pointer" },
  AssignProcessToJobObject: {
    parameters: ["pointer", "pointer"],
    result: "i32",
  },
  CloseHandle: { parameters: ["pointer"], result: "i32" },
  GetLastError: { parameters: [], result: "u32" },
} as const;

let kernel32: Deno.DynamicLibrary<typeof KERNEL32_SYMBOLS> | null = null;
let job: Deno.PointerValue = null;
let initAttempted = false;

/** Lazily open kernel32 and create the kill-on-close Job Object. Windows
 *  only; a no-op returning false everywhere else. Best-effort: any failure
 *  is logged and disables tracking. It never throws. */
function ensureJob(): boolean {
  if (Deno.build.os !== "windows") return false;
  if (job) return true;
  if (initAttempted) return false;
  initAttempted = true;
  try {
    const lib = Deno.dlopen("kernel32.dll", KERNEL32_SYMBOLS);
    const handle = lib.symbols.CreateJobObjectW(null, null);
    if (!handle) {
      log.warn(
        `CreateJobObjectW failed (err=${lib.symbols.GetLastError()}); ` +
          `sidecars may survive a hard core crash`,
      );
      lib.close();
      return false;
    }
    const info = new Uint8Array(EXT_LIMIT_INFO_SIZE);
    new DataView(info.buffer).setUint32(
      LIMIT_FLAGS_OFFSET,
      JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
      true, // little-endian
    );
    const ok = lib.symbols.SetInformationJobObject(
      handle,
      JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
      Deno.UnsafePointer.of(info),
      EXT_LIMIT_INFO_SIZE,
    );
    if (ok === 0) {
      log.warn(
        `SetInformationJobObject failed (err=${lib.symbols.GetLastError()}); ` +
          `sidecars may survive a hard core crash`,
      );
      lib.symbols.CloseHandle(handle);
      lib.close();
      return false;
    }
    kernel32 = lib;
    job = handle;
    log.info("job object created; sidecar PIDs will be tracked");
    return true;
  } catch (err) {
    log.warn(`jobctl FFI init failed: ${errMessage(err)}`);
    return false;
  }
}

/** Register a freshly-spawned sidecar PID with the kill-on-close job object
 *  so a hard core crash doesn't leave the sidecar running. Best-effort:
 *  failures are logged but do NOT throw. The sidecar still runs, it just
 *  loses orphan-cleanup protection. Windows only; no-op elsewhere. */
export function trackSidecarPid(pid: number): void {
  if (!ensureJob() || !job || !kernel32) return;
  try {
    const proc = kernel32.symbols.OpenProcess(PROCESS_ACCESS, 0, pid);
    if (!proc) {
      log.warn(`OpenProcess(${pid}) failed (err=${kernel32.symbols.GetLastError()})`);
      return;
    }
    const assigned = kernel32.symbols.AssignProcessToJobObject(job, proc);
    if (assigned === 0) {
      log.warn(
        `AssignProcessToJobObject(${pid}) failed ` + `(err=${kernel32.symbols.GetLastError()})`,
      );
    }
    kernel32.symbols.CloseHandle(proc);
  } catch (err) {
    log.warn(`jobctl track failed (pid=${pid}): ${errMessage(err)}`);
  }
}

/** Kept for call-site compatibility with the previous helper-based
 *  implementation. Intentionally a no-op: the Job Object handle must NOT be
 *  closed here. Closing it would immediately kill every sidecar, racing the
 *  sidecar manager's graceful shutdown. Core simply exiting closes the
 *  handle as its last act, which is the correct ordering. */
export async function shutdownJobctl(): Promise<void> {
  // Intentionally empty (see the doc comment above).
}
