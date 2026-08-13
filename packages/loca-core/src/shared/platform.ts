// Resolve the host's tool-platform tokens once, for per-tool OS gating.
//
// A tool declares the platforms it runs on in loca.json; the registry hides it
// everywhere on a host its list doesn't cover (see toolPlatformSupported). The
// only platform that needs runtime detection is Linux's display server: X11
// windows can be positioned from the command line, Wayland's can't, so a
// window-managing tool targets `linux_x11`. Core reads the session type here,
// once, so tool authors never touch env vars. The tokens are static for the
// life of the process, so the result is memoized.

import type { ToolPlatform } from "@loca/shared";

let cached: ToolPlatform[] | null = null;

// True when the current session is Wayland. WAYLAND_DISPLAY is set by every
// Wayland session and is the most reliable signal; XDG_SESSION_TYPE is the
// fallback for setups that don't export it.
export function isWayland(): boolean {
  if ((Deno.env.get("WAYLAND_DISPLAY") ?? "") !== "") return true;
  return (Deno.env.get("XDG_SESSION_TYPE") ?? "").toLowerCase() === "wayland";
}

// Pure mapping from OS + display server to the host's platform tokens. macOS ->
// ["darwin"], Windows -> ["windows"], Linux -> ["linux", "linux_x11" |
// "linux_wayland"].
export function resolvePlatforms(os: typeof Deno.build.os, wayland: boolean): ToolPlatform[] {
  switch (os) {
    case "darwin":
      return ["darwin"];
    case "windows":
      return ["windows"];
    default:
      return ["linux", wayland ? "linux_wayland" : "linux_x11"];
  }
}

// The platform tokens describing this host, memoized (they don't change for the
// life of the process).
export function hostPlatforms(): ToolPlatform[] {
  if (!cached) cached = resolvePlatforms(Deno.build.os, isWayland());
  return cached;
}

// Test-only: drop the memoized tokens so a test can re-resolve after stubbing
// the environment.
export function __resetForTesting(): void {
  cached = null;
}
