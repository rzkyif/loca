// Region-capture overlay logic. Standalone vanilla JS so the page can live
// outside SvelteKit and avoid the WebKit ESM TDZ bug that breaks SvelteKit's
// client bootstrap inside dev-mode secondary windows.
//
// IPC strategy: use `window.__TAURI_INTERNALS__.invoke` directly. That
// internal entry point is set up at webview boot regardless of
// `withGlobalTauri`, so it's always there. The higher-level
// `window.__TAURI__.event.emit` global has been observed to be flaky in
// dev. Falling back to a direct plugin invoke avoids depending on it.

const internals = window.__TAURI_INTERNALS__;
if (!internals || typeof internals.invoke !== "function") {
  console.error(
    "[region-capture] window.__TAURI_INTERNALS__.invoke missing; overlay can't talk to the host. Check that withGlobalTauri is true and that the page loaded inside a Tauri webview.",
  );
}

const invoke = (cmd, args) => internals.invoke(cmd, args);

// `emit` is just `invoke('plugin:event|emit', { event, payload })` under the
// hood (see node_modules/@tauri-apps/api/event.js). We inline that shape so
// the page never has to read the higher-level global.
const emit = (event, payload) =>
  invoke("plugin:event|emit", {
    event,
    payload: payload === undefined ? null : payload,
  });

const watermark = document.getElementById("watermark");
const selection = document.getElementById("selection");

// State machine handles BOTH capture gestures (per spec):
//   - mousedown → drag → mouseup           (pressed-drag)
//   - mousedown → mouseup → move → click   (click-drag-click; "click" =
//                                           mousedown+mouseup at the same
//                                           spot)
// The `armed` state holds while the first button is pressed but no drag has
// been detected yet. If the user releases without moving meaningfully it
// transitions to `tracking` (cursor follows mouse, selection rectangle live-
// updates, second click finalizes). If they move while pressed it transitions
// to `dragging` and finalizes on release.
const STATE_IDLE = "idle";
const STATE_ARMED = "armed";
const STATE_DRAGGING = "dragging";
const STATE_TRACKING = "tracking";
const DRAG_THRESHOLD_PX = 4;

let state = STATE_IDLE;
let startX = 0;
let startY = 0;
let currX = 0;
let currY = 0;
// Guard against double-finish from a fast mousedown→mouseup that triggers
// both the second-click finish (on mousedown) and the trailing mouseup.
let finished = false;

function dist(x1, y1, x2, y2) {
  const dx = x2 - x1;
  const dy = y2 - y1;
  return Math.sqrt(dx * dx + dy * dy);
}

function rect() {
  const x = Math.min(startX, currX);
  const y = Math.min(startY, currY);
  const w = Math.abs(startX - currX);
  const h = Math.abs(startY - currY);
  return { x, y, w, h };
}

function paintSelection() {
  const r = rect();
  selection.style.display = "block";
  selection.style.left = r.x + "px";
  selection.style.top = r.y + "px";
  selection.style.width = r.w + "px";
  selection.style.height = r.h + "px";
}

function resetUi() {
  state = STATE_IDLE;
  finished = false;
  selection.style.display = "none";
  watermark.style.display = "";
}

async function cancel() {
  if (finished) return;
  finished = true;
  resetUi();
  try {
    await emit("region-capture-cancelled");
  } catch (err) {
    console.error("[region-capture] cancel emit failed:", err);
  }
}

async function finish() {
  if (finished) return;
  const r = rect();
  if (r.w < 4 || r.h < 4) {
    // Effectively a misclick; treat as cancel.
    await cancel();
    return;
  }
  finished = true;
  const dpr = window.devicePixelRatio || 1;
  const x = Math.round(r.x * dpr);
  const y = Math.round(r.y * dpr);
  const width = Math.round(r.w * dpr);
  const height = Math.round(r.h * dpr);
  let monitorId = "primary";
  try {
    const id = await invoke("get_region_capture_target");
    if (id) monitorId = id;
  } catch (err) {
    console.warn("[region-capture] get_region_capture_target failed:", err);
  }
  try {
    const dataB64 = await invoke("capture_monitor_region", {
      monitorId,
      x,
      y,
      width,
      height,
    });
    await emit("region-capture-result", dataB64);
  } catch (err) {
    console.error("[region-capture] capture failed:", err);
    try {
      await emit("region-capture-cancelled");
    } catch (err2) {
      console.error("[region-capture] cancel emit failed:", err2);
    }
  }
  resetUi();
}

document.addEventListener(
  "mousedown",
  (e) => {
    if (e.button !== 0) return;
    if (state === STATE_IDLE) {
      startX = e.clientX;
      startY = e.clientY;
      currX = e.clientX;
      currY = e.clientY;
      state = STATE_ARMED;
      watermark.style.display = "none";
      paintSelection();
    } else if (state === STATE_TRACKING) {
      // Click-drag-click: the second mousedown ends the selection. Finish
      // here rather than on the trailing mouseup; feels snappier and
      // sidesteps any focus/event-order quirks. `finished` guards against
      // the trailing mouseup re-entering finish().
      currX = e.clientX;
      currY = e.clientY;
      void finish();
    }
  },
  true,
);

document.addEventListener(
  "mousemove",
  (e) => {
    if (state === STATE_ARMED) {
      currX = e.clientX;
      currY = e.clientY;
      if (dist(startX, startY, currX, currY) > DRAG_THRESHOLD_PX) {
        state = STATE_DRAGGING;
      }
      paintSelection();
    } else if (state === STATE_DRAGGING || state === STATE_TRACKING) {
      currX = e.clientX;
      currY = e.clientY;
      paintSelection();
    }
  },
  true,
);

document.addEventListener(
  "mouseup",
  (e) => {
    if (e.button !== 0) return;
    if (state === STATE_ARMED) {
      // Released without significant motion → click-drag-click first phase.
      state = STATE_TRACKING;
    } else if (state === STATE_DRAGGING) {
      currX = e.clientX;
      currY = e.clientY;
      void finish();
    }
  },
  true,
);

// Listen on both window and document to maximize the chance the key event
// is delivered (overlay is a transparent decorationless window; focus is
// fragile across platforms). Capture phase so we beat any default handling.
function onKeyDown(e) {
  if (e.key === "Escape") {
    e.preventDefault();
    void cancel();
  }
}
window.addEventListener("keydown", onKeyDown, true);
document.addEventListener("keydown", onKeyDown, true);

// Force focus to the body once the page mounts so keydown listeners receive
// events on macOS, where transparent always-on-top windows don't always
// auto-focus their document root.
document.addEventListener("DOMContentLoaded", () => {
  try {
    document.body.focus();
  } catch {
    // ignore: focus failures are harmless here
  }
});
