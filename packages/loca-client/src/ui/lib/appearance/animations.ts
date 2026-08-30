// Timing helpers for the app's UI animations. Every one honours
// `appearance.animationsEnabled` (0 duration when off) and
// `appearance.animationSpeedMultiplier` (percent; higher = faster).

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

// Suppresses entry animations during the session-restore burst, so loading a
// 50-message session does not kick off 50 simultaneous entrances.
let messageAnimationsReady = false;
// Stable msgIds already mounted, so the slide-in cannot replay when a wrapper
// remounts (load-more-history, stack regrouping). Recorded even while gated.
const messagesSeen = new Set<string>();
export function enableMessageAnimations() {
  messageAnimationsReady = true;
}

/** Whether the entry animation already ran, so the transcript staggers only the
 *  bubbles actually about to enter. */
export function hasMessageAnimated(msgId: string): boolean {
  return messagesSeen.has(msgId);
}

/** Whether a freshly mounted message wrapper should run its entry animation,
 *  recording the mount. The motion lives in the shared `MessageEnter`; this owns
 *  only the gating policy. A bubble with no stable id (a loading sentinel, the
 *  session bar) always animates. */
export function claimMessageEnter(msgId?: string): boolean {
  if (msgId) {
    if (messagesSeen.has(msgId)) return false;
    messagesSeen.add(msgId);
  }
  return messageAnimationsReady;
}

/** Animate a sidebar label's collapse/expand. The width-scaled speed and the DOM
 *  choreography live in the shared `collapseLabel`; this supplies only the
 *  duration policy, with `instant` skipping it entirely. */
export function applyLabelCollapse(el: HTMLElement, collapsed: boolean, instant: boolean): void {
  collapseLabel(el, collapsed, (baseMs) => (instant ? 0 : getDuration(baseMs)));
}

/** Per-frame expand/collapse. The motion lives in the shared `runExpand`; this
 *  resolves the settings-aware duration. */
export function runExpand(
  el: HTMLElement,
  direction: "open" | "close",
  onComplete?: () => void,
): ExpandHandle {
  return runExpandShared(el, direction, getDuration(), onComplete);
}
