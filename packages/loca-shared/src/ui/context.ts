import { getContext, setContext } from "svelte";
import { BASE_MS } from "./animations.ts";
import type { Alignment } from "./types.ts";
import { getDefaultSettings } from "../domain/settings/engine.ts";
import { hasAlpha } from "./color.ts";

// The state surface the extracted presentational components read. In the client
// a provider backs this with the live `settingsState` / `expansionState` stores
// (see lib/ui-context in the client); on the website a provider supplies static
// or demo values. A component never imports a client store directly: it calls
// `useUiContext()` and reads through this interface, so it renders identically
// in both apps and standalone (the DEFAULT_UI_CONTEXT below).
//
// Members that must stay reactive when the client mutates settings are declared
// as getters: reading `ctx.bubbleBlurRings` inside a component's `$derived`
// then tracks the underlying `$state`, because the provider's getter touches it.
//
// Every UiContext is built by `makeUiContext` (below). There is exactly one
// implementation of the settings-derived members, so the three call sites (the
// client provider, DEFAULT_UI_CONTEXT, and the website showcase) cannot drift
// the way a hand-copied object would.
export interface UiContext {
  /** The screen edge bubbles anchor to (settings.appearance alignment). */
  getAlignment(): Alignment;
  /** Whether the frosted bubble halo is on. */
  readonly bubbleBlurEnabled: boolean;
  /** How many concentric halo rings to render when blur is on. */
  readonly bubbleBlurRings: number;
  /** Whether Speech-to-Text is enabled (drives the composer's Voice Input
   *  button). The composer reads this so its DEFAULT rendition (no host
   *  override) matches the client at default settings, instead of each caller
   *  deciding the mic button's presence by hand. */
  readonly sttEnabled: boolean;
  /** Whether the model supports image input (`llm.supportImages`). Drives the
   *  composer's screen/region capture controls and the mobile image picker, so
   *  the DEFAULT rendition (no host override) matches the client: with image
   *  support off, capturing a screenshot would attach an image the model can't
   *  read, so those affordances are hidden. */
  readonly imagesEnabled: boolean;
  /** Optional per-system-message base color hex override (appearance setting),
   *  or undefined when no override is set. */
  readonly systemMessageDefaultColor: string | undefined;
  /** Resolve an animation duration in ms honoring the app's animation settings
   *  (master switch + speed multiplier). The static default returns base
   *  unchanged, so a standalone render animates at full BASE_MS speed. */
  animationDurationMs(baseMs?: number): number;
  /** Read shared per-id expansion state (e.g. an Expandable bubble header).
   *  `fallback` is returned when the id has no stored value yet, so a bubble
   *  that should start expanded (a tool `display`) and one that should start
   *  collapsed (a system prompt) both round-trip through the same registry. */
  expansionGet(id: string | undefined, fallback?: boolean): boolean;
  /** Write shared per-id expansion state. */
  expansionSet(id: string, value: boolean): void;
  /** Seed a per-id default ONLY if the id has no stored value yet. A bubble
   *  that opens expanded by default (a tool `display`) calls this on mount so
   *  the layout (which reads the registry raw) agrees with the body. Idempotent,
   *  so it never clobbers a user's toggle across the message-stack remounts. */
  expansionInit(id: string, value: boolean): void;
  /** The host the UI renders on. Views read this to pick layout variants; there
   *  is no web client, so the only values are the two native shells. */
  readonly platform: "desktop" | "mobile";
  /** The primary pointer. `"coarse"` (touch) Views gate hover-only affordances
   *  behind `pointer === "fine"`. */
  readonly pointer: "fine" | "coarse";
  /** Whether the host provides a system-level back gesture/button (Android),
   *  so in-app back / close affordances are redundant and Views drop them. False
   *  on desktop (keeps Esc + close buttons) and on iOS (no system back, so the
   *  on-screen back / close chrome stays). Set true only on Android. */
  readonly hasSystemBack: boolean;
  /** Register a back-navigation interceptor with the host (e.g. an open overlay
   *  wanting the Android back button / Esc to close it before the app navigates
   *  away). The handler returns `true` when it consumed the press. Returns a
   *  disposer; call it when the overlay closes. The client wires this to its
   *  back-handler registry; with no provider (website) it is an inert no-op, so
   *  a shared overlay never reaches into client code to own back. */
  registerBack(handler: () => boolean): () => void;
}

/** Vertical gap between chat bubbles, as a CSS length. With the frosted halo on,
 *  widen by twice the shadow distance on top of the tight default so adjacent
 *  bubbles' shadows/halos stay clear of each other instead of touching; with
 *  blur off keep the tight default (the old gap-2). Reads the reactive
 *  `bubbleBlurEnabled` getter, so applying it via `style:gap={bubbleGap(ui)}`
 *  stays live when the setting toggles. `--bubble-shadow-distance` cascades
 *  globally (the client writes it from appearance.bubbleShadowDistance; the
 *  website inherits the base default). */
export function bubbleGap(ui: UiContext): string {
  return `calc(${bubbleGapExpr(ui)})`;
}

/** The bare CSS expression behind `bubbleGap` (no `calc()` wrapper), so callers
 *  can compose it into a larger `calc()` (e.g. a merged bubble's overlap margin,
 *  which is `-(gap + padding)`). Mobile bubbles have no shadow/halo (see
 *  Bubble.svelte), so they need no extra clearance: keep the tight default gap. */
export function bubbleGapExpr(ui: UiContext): string {
  return ui.bubbleBlurEnabled && ui.platform !== "mobile"
    ? "0.5rem + 2 * var(--bubble-shadow-distance)"
    : "0.5rem";
}

const KEY = Symbol("loca:ui-context");

/** The host-specific pieces a `makeUiContext` caller supplies. Only `getSetting`
 *  is required; everything else has a presentational default so a no-provider
 *  render (the website gallery, a standalone preview) still works. */
export interface UiContextSources {
  /** Read a settings value by key. Live in the client, schema-default elsewhere.
   *  The factory wraps each read in a getter, so a live store stays reactive. */
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

/** Build a `UiContext` from host-specific sources. The settings-derived members
 *  (alignment, blur, the system-message tint) are computed here ONCE, so the
 *  client provider, DEFAULT_UI_CONTEXT, and the showcase share identical logic
 *  and cannot drift. Settings reads happen inside getters, so a live store read
 *  by a consumer's `$derived` stays reactive. */
export function makeUiContext(sources: UiContextSources): UiContext {
  const { getSetting } = sources;
  return {
    // Mobile is a single fullscreen activity with no window alignment, so the
    // chat column and every non-message bubble center; the chat message Views
    // override per sender (user right, agent left). Desktop follows the setting.
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

/** Used when no provider is mounted: a standalone render of a single component
 *  (the website's static showcases, a Storybook-like preview). Every value is
 *  read from the settings schema's defaults via `getDefaultSettings()`, so a
 *  no-provider render matches a fresh app EXACTLY (e.g. center alignment, 1 halo
 *  ring) instead of drifting from hardcoded guesses. */
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
