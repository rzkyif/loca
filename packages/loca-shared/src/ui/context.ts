import { getContext, setContext } from "svelte";
import { BASE_MS } from "./animations.ts";
import type { Alignment } from "./types.ts";
import { getDefaultSettings } from "../domain/settings/engine.ts";
import { hasAlpha } from "./color.ts";

// The state surface the shared presentational components read: the client backs
// it with live stores, the website with static values, and a no-provider render
// falls back to DEFAULT_UI_CONTEXT. Settings-derived members are getters so a
// consumer's `$derived` tracks the underlying `$state`.
export interface UiContext {
  /** The screen edge bubbles anchor to (settings.appearance alignment). */
  getAlignment(): Alignment;
  /** Whether the frosted bubble halo is on. */
  readonly bubbleBlurEnabled: boolean;
  /** How many concentric halo rings to render when blur is on. */
  readonly bubbleBlurRings: number;
  /** Whether Speech-to-Text is enabled (drives the composer's Voice Input
   *  button). */
  readonly sttEnabled: boolean;
  /** Whether the model supports image input (`llm.supportImages`); gates the
   *  capture controls and the mobile image picker. */
  readonly imagesEnabled: boolean;
  /** Per-system-message base color hex override, or undefined when unset. */
  readonly systemMessageDefaultColor: string | undefined;
  /** Resolve an animation duration in ms honoring the app's animation settings.
   *  The static default returns base unchanged. */
  animationDurationMs(baseMs?: number): number;
  /** Read shared per-id expansion state, returning `fallback` when unset. */
  expansionGet(id: string | undefined, fallback?: boolean): boolean;
  /** Write shared per-id expansion state. */
  expansionSet(id: string, value: boolean): void;
  /** Seed a per-id default ONLY if the id has no stored value yet, so a toggle
   *  survives the message-stack remounts. */
  expansionInit(id: string, value: boolean): void;
  readonly platform: "desktop" | "mobile";
  /** The primary pointer. Views gate hover-only affordances on `"fine"`. */
  readonly pointer: "fine" | "coarse";
  /** Whether the host has a system back gesture/button (Android only), making
   *  in-app back / close affordances redundant. */
  readonly hasSystemBack: boolean;
  /** Register a back-navigation interceptor (returns `true` when it consumed
   *  the press). Returns a disposer; inert with no provider. */
  registerBack(handler: () => boolean): () => void;
}

/** Vertical gap between chat bubbles, as a CSS length. */
export function bubbleGap(ui: UiContext): string {
  return `calc(${bubbleGapExpr(ui)})`;
}

/** The bare expression behind `bubbleGap` (no `calc()` wrapper), composable into
 *  a larger `calc()`. With the halo on, widen by twice the shadow distance so
 *  adjacent halos stay clear; mobile bubbles have no halo. */
export function bubbleGapExpr(ui: UiContext): string {
  return ui.bubbleBlurEnabled && ui.platform !== "mobile"
    ? "0.5rem + 2 * var(--bubble-shadow-distance)"
    : "0.5rem";
}

const KEY = Symbol("loca:ui-context");

/** The host-specific pieces a `makeUiContext` caller supplies. Only `getSetting`
 *  is required; everything else has a presentational default. */
export interface UiContextSources {
  /** Read a settings value by key. Live in the client, schema-default elsewhere. */
  getSetting(key: string): unknown;
  /** Resolve an animation duration honoring app settings. Default: identity. */
  animationDurationMs?: (baseMs?: number) => number;
  /** Read shared per-id expansion state. Default: return the fallback. */
  expansionGet?: (id: string | undefined, fallback?: boolean) => boolean;
  /** Write shared per-id expansion state. Default: no-op. */
  expansionSet?: (id: string, value: boolean) => void;
  /** Seed a per-id default only if absent. Default: no-op. */
  expansionInit?: (id: string, value: boolean) => void;
  platform?: "desktop" | "mobile";
  pointer?: "fine" | "coarse";
  /** Whether the host has a system back gesture (Android). Default: false. */
  hasSystemBack?: boolean;
  /** Register a back-navigation interceptor. Default: inert no-op disposer. */
  registerBack?: (handler: () => boolean) => () => void;
}

/** Build a `UiContext` from host-specific sources. */
export function makeUiContext(sources: UiContextSources): UiContext {
  const { getSetting } = sources;
  return {
    // Mobile is a single fullscreen activity with no window alignment.
    getAlignment: () =>
      (sources.platform ?? "desktop") === "mobile"
        ? "center"
        : ((getSetting("layout.alignment") as Alignment | undefined) ?? "center"),
    get bubbleBlurEnabled() {
      return getSetting("appearance.bubbleBlurEnabled") !== false;
    },
    get bubbleBlurRings() {
      return (getSetting("appearance.bubbleBlurRings") as number | undefined) ?? 1;
    },
    get sttEnabled() {
      return getSetting("stt.enabled") !== false;
    },
    get imagesEnabled() {
      return getSetting("llm.supportImages") === true;
    },
    get systemMessageDefaultColor() {
      // A fully transparent value means "no override" (fall back to the base).
      const c = getSetting("appearance.systemMessageDefaultColor") as string | undefined;
      return hasAlpha(c) ? c : undefined;
    },
    animationDurationMs: sources.animationDurationMs ?? ((baseMs = BASE_MS) => baseMs),
    expansionGet: sources.expansionGet ?? ((_id, fallback = false) => fallback),
    expansionSet: sources.expansionSet ?? (() => {}),
    expansionInit: sources.expansionInit ?? (() => {}),
    platform: sources.platform ?? "desktop",
    pointer: sources.pointer ?? "fine",
    hasSystemBack: sources.hasSystemBack ?? false,
    registerBack: sources.registerBack ?? (() => () => {}),
  };
}

/** Used when no provider is mounted, seeded from the settings schema's defaults
 *  so a standalone render matches a fresh app exactly. */
const D = getDefaultSettings();
export const DEFAULT_UI_CONTEXT: UiContext = makeUiContext({
  getSetting: (key) => D[key],
});

/** Mount a context for the subtree. Call once high in each app's tree. */
export function setUiContext(ctx: UiContext): void {
  setContext(KEY, ctx);
}

/** Read the mounted context, falling back to DEFAULT_UI_CONTEXT. */
export function useUiContext(): UiContext {
  return getContext<UiContext | undefined>(KEY) ?? DEFAULT_UI_CONTEXT;
}
