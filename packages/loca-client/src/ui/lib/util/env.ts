/**
 * Helpers for figuring out which environment the frontend is running in.
 * Use these instead of casting `(window as any)` at every check site.
 */

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

/**
 * True when the app is running inside the Tauri desktop shell.
 *
 * Safe to call in any context (SSR, web, desktop); returns `false` when
 * `window` is undefined or the Tauri marker is missing.
 */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}
