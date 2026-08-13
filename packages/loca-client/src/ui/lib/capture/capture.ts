/**
 * Take screenshots of the user's monitors. Lists available monitors and
 * captures one as a base64 PNG, briefly hiding the app window first so it
 * doesn't show up in the screenshot.
 *
 * Also exposes a region-capture flow that opens a transparent fullscreen
 * overlay window on the active monitor and returns the user-selected
 * rectangle as a base64 PNG. Region capture is event-driven: the Rust
 * overlay window draws the selection and emits `region-capture-result` /
 * `region-capture-cancelled` events. There's no direct "capture this
 * rectangle" call from the JS side.
 *
 * All platform-specific Tauri calls go through `$lib/platform/`, so the
 * same code paths can run under the web/mobile stub when those builds
 * land. Web builds rely on the platform's no-op fallbacks (capture itself
 * isn't available without the host process).
 */

import { type MonitorInfo, platform } from "$lib/platform";
import { getLogger } from "$lib/util/log";

const log = getLogger("capture");

// Alias kept so consumers don't have to import from `$lib/platform`. The
// shape is the canonical `MonitorInfo` returned by the platform's capture
// layer.
export type CaptureMonitorInfo = MonitorInfo;

export async function listCaptureMonitors(): Promise<CaptureMonitorInfo[]> {
  try {
    return await platform().capture.monitors();
  } catch (e) {
    log.warn("Failed to list monitors:", e);
    return [];
  }
}

/** Capture a monitor as a base64 PNG. Hides the app window during the capture
 *  if it was visible, and restores it afterwards. Returns null on failure. */
export async function captureMonitor(monitorId: string): Promise<string | null> {
  const shouldHide = await platform().windowing.isVisible();

  try {
    if (shouldHide) {
      await platform().windowing.hide();
      // Compositor lag: after hide() resolves, the window may still be on
      // screen on slow compositors (notably tiling WMs on X11). 180 ms
      // empirically covers observed cases without noticeable capture delay.
      await new Promise((r) => setTimeout(r, 180));
    }
    return await platform().capture.captureMonitor(monitorId);
  } catch (e) {
    log.error("Failed to capture monitor:", e);
    return null;
  } finally {
    if (shouldHide) {
      try {
        await platform().windowing.show();
      } catch (e) {
        log.warn("Failed to restore window:", e);
      }
    }
  }
}

/** Show a fullscreen transparent overlay on the active monitor, let the user
 *  drag a rectangle, capture that region, and return the base64 PNG. Returns
 *  null on cancellation (ESC, click outside, or capture failure).
 *
 *  Hides the main window for the duration so the overlay sits alone. The
 *  overlay window is positioned and shown by a Rust command. Keeps the
 *  logical/physical pixel math out of the JS side, where it'd have to wrestle
 *  with macOS retina + multi-monitor edge cases.
 */
export async function captureRegion(): Promise<string | null> {
  const mainWasVisible = await platform().windowing.isVisible();

  let unsubscribe: (() => void) | null = null;

  try {
    if (mainWasVisible) {
      await platform().windowing.hide();
      // Compositor lag, see captureMonitor()'s comment.
      await new Promise((r) => setTimeout(r, 180));
    }

    // Wire the result listener BEFORE showing the overlay: a fast drag-
    // release or ESC press on the page could otherwise fire before the
    // listener attaches and the event would be lost.
    let resolveResult!: (v: string | null) => void;
    const resultPromise = new Promise<string | null>((r) => {
      resolveResult = r;
    });
    let settled = false;
    const settle = (v: string | null) => {
      if (settled) return;
      settled = true;
      resolveResult(v);
    };
    unsubscribe = await platform().capture.subscribeRegionResult(settle);

    // Position + size + show the overlay in one Rust call. Returns the xcap
    // monitor id that matches the active monitor; we stash it so the
    // overlay's screen capture targets the right display.
    const xcapMonitorId = await platform().capture.showRegionOverlay();
    try {
      await platform().capture.setRegionTarget(xcapMonitorId);
    } catch (e) {
      log.warn("setRegionTarget failed:", e);
    }

    return await resultPromise;
  } catch (e) {
    log.error("region capture failed:", e);
    return null;
  } finally {
    if (unsubscribe) unsubscribe();
    try {
      await platform().capture.hideRegionOverlay();
    } catch {
      // ignore
    }
    if (mainWasVisible) {
      try {
        await platform().windowing.show();
      } catch (e) {
        log.warn("restore main window failed:", e);
      }
    }
  }
}
