/**
 * The desktop/mobile window slide engine: the imperative state machine that
 * moves the transparent bubble window (and, on mobile, cross-slides screens)
 * during boot reveal, shortcut show/hide, and panel/screen navigation.
 *
 * Per the composable convention, this class holds the transient imperative
 * state (the bound element refs, the in-flight guards, and the reactive mobile
 * exit-overlay mode) plus the methods that drive the transitions; the consumer
 * (the route) owns the lifecycle - it binds the elements via `bind:this`, calls
 * these methods from its boot `onMount` and the visibility/hide/monitor
 * subscriptions, and keeps the two `$effect`s that trigger `runSlide` (on a
 * pending navigation) and `positionWindow` (on alignment/monitor/width change).
 *
 * Two motion mechanisms live here. The panel swap (`runSlide`, desktop) drives
 * the layer transform through the shared `animateTransform` (Web Animations API,
 * explicit keyframes) so the slide-in always starts from the off-screen edge and
 * never from a stale mid-transform. The window-level reveal/hide
 * (`applyWindowState`) and the mobile carousel still use the CSS-transition
 * pattern - set the transition, then the target style, source value already on
 * the element - which is timing-sensitive on WKWebView and only observable in the
 * running app; the mobile carousel adds a double-`rAF` park so the WebView paints
 * the start state before the transition turns on.
 */

import { tick } from "svelte";
import { platform } from "$lib/platform";
import { settingsState } from "$stores/settings.svelte";
import { viewState, type AppMode } from "$stores/view.svelte";
import { windowTransition } from "$stores/shortcut.svelte";
import { getDuration } from "$lib/appearance/animations";
import { animateTransform } from "@loca/shared/ui/animations";
import { getLogger } from "$lib/util/log";

const windowLog = getLogger("window");

const TRANSITION_EASING = "cubic-bezier(0.4, 0, 0.2, 1)";

// First launch parks the content offscreen, reveals the (transparent)
// window, then waits this long before sliding the content in, giving the
// freshly mounted UI a beat to settle so the slide starts from a stable
// layout instead of shifting mid-animation.
const BOOT_SHOW_DELAY_MS = 1000;

function offscreenTransform(alignment: "left" | "center" | "right"): string {
  if (alignment === "left") return "translateX(-100%)";
  if (alignment === "right") return "translateX(100%)";
  return "translateY(100%)";
}

export class WindowChoreography {
  // Element refs, bound by the consumer via `bind:this`. `container` is the
  // desktop scrolling `main`; `panelLayer` is the viewport-sized slide wrapper;
  // `mobileFrameEl`/`mobileExitEl` are the mobile carousel's committed frame and
  // transient exit overlay.
  container = $state<HTMLElement | undefined>(undefined);
  panelLayer = $state<HTMLElement | undefined>(undefined);
  mobileFrameEl = $state<HTMLElement | undefined>(undefined);
  mobileExitEl = $state<HTMLElement | undefined>(undefined);

  // Mobile carousel state. During a screen change the outgoing screen is rendered
  // as a fixed exit overlay (`slideOutMode`) that pages off one edge while the
  // committed frame pages in from the other, so the two move TOGETHER (a true
  // cross-slide) rather than one sliding out, a blank beat, then the next sliding
  // in. The committed frame is the only mount of the incoming screen (no double
  // load); the overlay is a transient render of the outgoing screen for its exit.
  slideOutMode = $state<AppMode | null>(null);

  private panelToggling = false;
  private hidingInFlight = false;

  // The route's navigation $effect early-outs while a slide is mid-flight (the
  // tail of runSlide re-runs itself for any navigation queued during it), so it
  // reads this instead of calling into the self-guarded runSlide redundantly.
  get sliding(): boolean {
    return this.panelToggling;
  }

  // On mobile the app is a single opaque fullscreen activity: there is no
  // transparent window to slide, so the window-level reveal is a no-op and the
  // navigation slide uses the mobile carousel branch instead.
  constructor(private readonly onMobile: boolean) {}

  // Imperatively drive the window-level slide on `container`. Mirrors the
  // same JS+CSS pattern used for panel swap (`runSlide`): set transition then
  // target style; the WKWebView transition fires reliably because the source
  // value is already on the element.
  //   - "visible":   on screen (transform cleared) and opaque.
  //   - "offscreen": slid out per alignment and held opaque, so the slide
  //                  reads as pure motion with no fade in either direction.
  // First paint inlines opacity:0 directly on the element so the window
  // doesn't flash visible before the first applyWindowState() runs; the first
  // "offscreen" call lifts it to 1 while the content is safely off-screen.
  applyWindowState(state: "visible" | "offscreen", animate: boolean) {
    if (!this.container) return;
    // Mobile has no floating window to slide off screen: keep the content
    // always visible (and lift the first-paint opacity:0) regardless of the
    // requested state, so the boot reveal sequence below is a no-op visually.
    if (this.onMobile) {
      this.container.style.transition = "";
      this.container.style.transform = "";
      this.container.style.opacity = "1";
      return;
    }
    const dur = animate ? getDuration() : 0;
    this.container.style.transition =
      dur > 0
        ? `transform ${dur}ms ${TRANSITION_EASING}, opacity ${dur}ms ${TRANSITION_EASING}`
        : "";
    if (state === "visible") {
      this.container.style.transform = "";
      this.container.style.opacity = "1";
    } else {
      // Hold opacity at 1 so the window slides without fading (a full-100%
      // translate already clears it from the viewport). Setting it explicitly
      // also lifts the first-paint opacity:0 so the initial boot slide-in is
      // pure motion rather than a fade.
      this.container.style.opacity = "1";
      this.container.style.transform = offscreenTransform(settingsState.getAlignment());
    }
  }

  async animateHideThenHide() {
    if (this.hidingInFlight) return;
    this.hidingInFlight = true;
    windowTransition.begin();
    this.applyWindowState("offscreen", true);
    try {
      await new Promise((r) => setTimeout(r, getDuration()));
      await platform().windowing.hide();
    } catch (e) {
      windowLog.warn("hide failed:", e);
    } finally {
      this.hidingInFlight = false;
      windowTransition.end();
    }
  }

  // Tail of the first-launch reveal: the route's onMount finally has already
  // parked the content offscreen, opened the `windowTransition` guard, and
  // shown the window. After a settle beat, slide the content in with the same
  // animation as a shortcut-driven show, then close the guard once it lands.
  async revealAfterSettle() {
    await new Promise((r) => setTimeout(r, BOOT_SHOW_DELAY_MS));
    this.applyWindowState("visible", true);
    await new Promise((r) => setTimeout(r, getDuration()));
    windowTransition.end();
  }

  async positionWindow() {
    try {
      await platform().windowing.position({
        monitorId: settingsState.getMonitor(),
        alignment: settingsState.getAlignment(),
        width: (settingsState.currentSettings["layout.width"] as number | undefined) ?? 700,
      });
    } catch (e) {
      windowLog.error("Failed to position window", e);
    }
  }

  async runSlide() {
    if (this.panelToggling) return;
    this.panelToggling = true;

    // On mobile, Quick Settings is a bottom-sheet overlay over the chat, not a
    // panel swap: commit with no slide so the sheet's own transition animates it
    // in/out while the chat stays put behind it.
    const overlayOnly =
      this.onMobile &&
      (viewState.pendingMode === "quickSettings" || viewState.mode === "quickSettings");
    const dur = overlayOnly ? 0 : getDuration();
    const layer = this.panelLayer;

    if (dur > 0 && this.onMobile) {
      // Mobile carousel: a TRUE cross-slide. The outgoing screen (rendered as the
      // exit overlay) pages off one edge while the incoming screen (the committed
      // frame) pages in from the opposite edge, both moving together. Leaving chat
      // (going deeper) pages forward (old exits left, new enters right); returning
      // to chat pages back (old exits right, new enters left).
      const forward = viewState.pendingMode !== "chat";
      const outX = forward ? "-100%" : "100%"; // outgoing screen leaves toward
      const inX = forward ? "100%" : "-100%"; // incoming screen enters from

      // Mount the outgoing screen as the exit overlay, then commit so the frame
      // renders the incoming screen; one tick later both panes are in the DOM.
      this.slideOutMode = viewState.mode;
      viewState.commit();
      await tick();
      const frame = this.mobileFrameEl;
      const exit = this.mobileExitEl;
      if (frame && exit) {
        // Park: incoming frame off on the entering edge, outgoing overlay at rest,
        // transitions OFF. Two rAFs guarantee the WebView paints the parked state
        // before the transition turns on, so the motion starts from the edge
        // instead of jumping mid-screen.
        frame.style.transition = "none";
        frame.style.transform = `translateX(${inX})`;
        exit.style.transition = "none";
        exit.style.transform = "translateX(0)";
        await new Promise<void>((r) =>
          requestAnimationFrame(() => requestAnimationFrame(() => r())),
        );

        // Slide both at once: frame to rest, overlay off the far edge.
        frame.style.transition = `transform ${dur}ms ${TRANSITION_EASING}`;
        frame.style.transform = "translateX(0)";
        exit.style.transition = `transform ${dur}ms ${TRANSITION_EASING}`;
        exit.style.transform = `translateX(${outX})`;
        await new Promise((r) => setTimeout(r, dur));

        frame.style.transition = "";
        frame.style.transform = "";
      }
      // Drop the overlay; the committed frame is the resting screen.
      this.slideOutMode = null;
    } else if (layer && dur > 0) {
      const offscreen = offscreenTransform(settingsState.getAlignment());

      // Phase 1: slide the wrapper (and everything inside) to the aligned edge.
      await animateTransform(layer, "none", offscreen, dur, TRANSITION_EASING);

      // Phase 2: commit the mode swap while offscreen, so the new mode
      // mounts already offscreen with no extra positioning.
      viewState.commit();
      await tick();

      // Phase 3: slide the wrapper back in from the same edge to its natural
      // position. WAAPI keyframes start the motion at `offscreen` explicitly, so
      // the slide-in can't begin from a stale mid-transform (the "starts from the
      // middle of the screen" glitch the timer + CSS-transition version had).
      await animateTransform(layer, offscreen, "none", dur, TRANSITION_EASING);
      layer.style.transform = "";
    } else {
      viewState.commit();
    }

    this.panelToggling = false;
    if (viewState.mode === "chat") this.scrollToBottom();
    // pendingMode may have changed again mid-slide (rapid navigation): re-run.
    if (viewState.pendingMode !== viewState.mode) void this.runSlide();
  }

  async scrollToBottom() {
    await tick();
    if (this.container) {
      this.container.scrollTo({ top: this.container.scrollHeight, behavior: "instant" });
    }
  }
}
