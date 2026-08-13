/**
 * Transition factories and timing helpers for the app's UI animations.
 * All factories honour the two Appearance settings:
 *   - `appearance.animationsEnabled` (master switch; duration becomes 0 when off)
 *   - `appearance.animationSpeedMultiplier` (percent; higher = faster)
 */

import { settingsState } from "$stores";
import {
  BASE_MS,
  collapseLabel,
  type ExpandHandle,
  runExpand as runExpandShared,
} from "@loca/shared/ui/animations";

export { BASE_MS, type ExpandHandle };

export const CSS_EASING = "cubic-bezier(0.4, 0, 0.2, 1)";

export function getDuration(ms: number = BASE_MS): number {
  if (!settingsState.currentSettings["appearance.animationsEnabled"]) return 0;
  const mult =
    (settingsState.currentSettings["appearance.animationSpeedMultiplier"] as number) ?? 100;
  if (!mult || mult <= 0) return ms;
  return (ms * 100) / mult;
}

// Gate used by +page.svelte to suppress message entry animations during the
// initial session-restore burst. Without this, loading a 50-message session
// would kick off 50 simultaneous entry animations.
let messageAnimationsReady = false;
// Stable msgIds we've already mounted. Prevents the slide-in from replaying
// on remount (e.g. load-more-history reshuffles wrappers, or stack regrouping
// moves a bubble under a new parent). Recorded even while gated so the
// post-gate state already knows about every mounted message.
const messagesSeen = new Set<string>();
export function enableMessageAnimations() {
  messageAnimationsReady = true;
}

/** Whether the message's entry animation already ran (or was recorded while
 *  gated). Lets the transcript compute stagger delays for only the bubbles
 *  that are actually about to enter. */
export function hasMessageAnimated(msgId: string): boolean {
  return messagesSeen.has(msgId);
}

/**
 * Decide whether a freshly mounted message wrapper should run its entry
 * animation, and record the mount. The motion itself lives in the shared
 * `MessageEnter` component (`@loca/shared/ui`); this owns only the client's
 * gating policy:
 *   - `msgId` dedupes replays: a bubble that's already animated stays animated
 *     even if its parent reshuffles (e.g. MessageStackGroup regroups). A bubble
 *     with no stable id (a loading sentinel, the session bar) always animates.
 *   - During the session-restore burst `messageAnimationsReady` is false, so
 *     restored bubbles appear without 50 simultaneous entrances; their ids are
 *     still recorded so they never animate retroactively once the gate opens.
 */
export function claimMessageEnter(msgId?: string): boolean {
  if (msgId) {
    if (messagesSeen.has(msgId)) return false;
    messagesSeen.add(msgId);
  }
  return messageAnimationsReady;
}

/**
 * Imperatively animate a label's horizontal slide between visible (natural
 * width, opacity 1) and collapsed (width 0, opacity 0). Same shape as the
 * panel slide in +page.svelte: set transition, then assign the target style.
 *   - `instant`: skip the animation (first mount, or animations disabled).
 * Width is measured via `scrollWidth` while temporarily releasing the
 * explicit width; this lets the same span handle dynamic content (e.g.
 * "Downloads" → "Downloading…") without caching a stale natural width.
 */
/**
 * Animate a sidebar label's collapse/expand. The width-scaled speed and the DOM
 * choreography live in the shared `collapseLabel`; here we only supply the app's
 * duration policy: `instant` (first mount / animations off) skips it, otherwise
 * the natural ms passes through `getDuration` (master switch + speed multiplier).
 */
export function applyLabelCollapse(el: HTMLElement, collapsed: boolean, instant: boolean): void {
  collapseLabel(el, collapsed, (baseMs) => (instant ? 0 : getDuration(baseMs)));
}

/**
 * Per-frame expand/collapse. The motion lives in the shared `runExpand`; here we
 * only resolve the app's settings-aware duration. Kept so existing client
 * callers (`$lib/appearance/animations`) need no import change.
 */
export function runExpand(
  el: HTMLElement,
  direction: "open" | "close",
  onComplete?: () => void,
): ExpandHandle {
  return runExpandShared(el, direction, getDuration(), onComplete);
}
