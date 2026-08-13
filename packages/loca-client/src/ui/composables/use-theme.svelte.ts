/**
 * Applies appearance/layout settings to documentElement: the dark-mode class,
 * the rem-base font size, bubble/accent color tokens (with theme-adaptive dark
 * inversions), corner radii, fonts, and shadow tokens. Also bridges the OS
 * dark/light preference via matchMedia.
 *
 * Convention mirrors use-blink: this class owns the DOM-applying mechanism; the
 * consuming component owns the $effects that read settings and call these
 * methods, the `loaded` gate, and the onMount/onDestroy lifecycle (applyAll on
 * the boot critical path, listenSystemTheme + its teardown).
 */
import { darkFromLight } from "$lib/appearance/color";

// Fallback stacks appended after the user's chosen family so a missing
// glyph (or a typo'd / uninstalled face) gracefully degrades to the
// platform-native stack rather than the browser default.
const FONT_DEFAULT_FALLBACK = `-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Oxygen, Ubuntu, Cantarell, sans-serif`;
const FONT_MONO_FALLBACK = `ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace`;

export class Theme {
  // SSR is off (see +layout.ts) so it's safe to touch `window`/`document` at
  // construction time.
  private mql = window.matchMedia("(prefers-color-scheme: dark)");

  applyTheme(theme: string): void {
    const isDark = theme === "dark" || (theme === "auto" && this.mql.matches);
    document.documentElement.classList.toggle("dark", isDark);
  }

  applyTextSize(size: number): void {
    document.documentElement.style.fontSize = `${size}px`;
  }

  applyBubbleColor(cssVar: string, hex: string | undefined): void {
    if (typeof hex !== "string" || hex.length === 0) return;
    document.documentElement.style.setProperty(cssVar, hex);
  }

  applyCssVarPx(cssVar: string, value: number | undefined): void {
    if (typeof value !== "number" || !Number.isFinite(value)) return;
    document.documentElement.style.setProperty(cssVar, `${value}px`);
  }

  // Theme-adaptive color: write the stored light-mode hex to `lightVar` and its
  // theme inversion (color.ts `darkFromLight`, the reversible stepping curve) to
  // `darkVar`, so the `.dark` rules render the flipped color. The same curve
  // backs the picker round-trip, so what's stored, previewed, and rendered agree.
  setThemeColor(lightVar: string, darkVar: string, hex: string | undefined): void {
    if (typeof hex !== "string" || hex.length === 0) return;
    document.documentElement.style.setProperty(lightVar, hex);
    document.documentElement.style.setProperty(darkVar, darkFromLight(hex));
  }

  applyFont(cssVar: "--font-default" | "--font-mono", value: unknown): void {
    const family = typeof value === "string" ? value : "";
    if (!family || family === "default") {
      document.documentElement.style.removeProperty(cssVar);
      return;
    }
    const fallback = cssVar === "--font-mono" ? FONT_MONO_FALLBACK : FONT_DEFAULT_FALLBACK;
    const escaped = family.replace(/"/g, '\\"');
    document.documentElement.style.setProperty(cssVar, `"${escaped}", ${fallback}`);
  }

  // Apply every appearance/layout DOM setting from the (client-local) settings.
  // Runs on the boot critical path before show so the window paints correctly
  // themed; the per-key $effects in the consumer re-apply on any later change.
  applyAll(settings: Record<string, unknown>): void {
    this.applyTheme((settings["appearance.theme"] as string | undefined) ?? "auto");
    this.applyTextSize((settings["appearance.textSize"] as number | undefined) ?? 16);
    this.setThemeColor(
      "--user-bubble-bg-light",
      "--user-bubble-bg-dark",
      settings["appearance.userBubbleColor"] as string | undefined,
    );
    this.setThemeColor(
      "--agent-bubble-bg-light",
      "--agent-bubble-bg-dark",
      settings["appearance.agentBubbleColor"] as string | undefined,
    );
    this.setThemeColor(
      "--agent2-bubble-bg-light",
      "--agent2-bubble-bg-dark",
      settings["appearance.secondaryAgentBubbleColor"] as string | undefined,
    );
    this.applyBubbleColor(
      "--default-base",
      settings["appearance.defaultColor"] as string | undefined,
    );
    this.applyBubbleColor(
      "--accent-red-base",
      settings["appearance.accentRed"] as string | undefined,
    );
    this.applyBubbleColor(
      "--accent-blue-base",
      settings["appearance.accentBlue"] as string | undefined,
    );
    this.applyBubbleColor(
      "--accent-purple-base",
      settings["appearance.accentPurple"] as string | undefined,
    );
    this.applyBubbleColor(
      "--accent-green-base",
      settings["appearance.accentGreen"] as string | undefined,
    );
    this.applyBubbleColor(
      "--accent-yellow-base",
      settings["appearance.accentYellow"] as string | undefined,
    );
    this.applyCssVarPx("--rounded-small", settings["appearance.roundedSmall"] as number);
    this.applyCssVarPx("--rounded-medium", settings["appearance.roundedMedium"] as number);
    this.applyCssVarPx("--rounded-large", settings["appearance.roundedLarge"] as number);
    this.applyFont("--font-default", settings["appearance.defaultFont"]);
    this.applyFont("--font-mono", settings["appearance.monoFont"]);
    this.setThemeColor(
      "--bubble-shadow-color-light",
      "--bubble-shadow-color-dark",
      settings["appearance.bubbleShadowColor"] as string | undefined,
    );
    this.applyCssVarPx(
      "--bubble-shadow-distance",
      settings["appearance.bubbleShadowDistance"] as number,
    );
  }

  // Subscribe to OS dark/light changes; returns the teardown.
  listenSystemTheme(callback: () => void): () => void {
    this.mql.addEventListener("change", callback);
    return () => this.mql.removeEventListener("change", callback);
  }
}

export function useTheme(): Theme {
  return new Theme();
}
