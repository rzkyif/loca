/**
 * Color helpers for the appearance system. Colors are stored as CSS
 * `oklch(L C H / A)` strings, NOT sRGB hex: that keeps values exact end to end
 * (no 8-bit quantization, no gamut clamping of the stored value -- chroma beyond
 * the sRGB gamut is preserved and the browser gamut-maps only at paint, so
 * wide-gamut displays benefit automatically). `parseColor` still accepts legacy
 * hex (and pasted hex) and the per-shade `oklch(from ...)` ladders in app.css
 * consume either form, so old settings keep working.
 *
 * `darkFromLight` / `lightFromDark` are the single, theme-wide color inversion:
 * a reversible map that follows the neutral surface stepping curve so custom
 * colors flip between themes the same way the UI surfaces do.
 *
 * Reference: https://bottosson.github.io/posts/oklab/
 */

export interface Oklch {
  l: number;
  c: number;
  h: number;
  /** alpha 0..1 */
  a: number;
}

/** Accepts `#rrggbb` (alpha defaulted to 1) or `#rrggbbaa`. */
const HEX_RE = /^#([0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/;

export function isValidHex(s: string): boolean {
  return HEX_RE.test(s);
}

/** Normalize any valid hex to 8-char form (`#rrggbbaa`). */
export function toEightCharHex(hex: string): string {
  if (!HEX_RE.test(hex)) throw new Error(`invalid hex: ${hex}`);
  return hex.length === 7 ? `${hex}ff` : hex.toLowerCase();
}

function srgbToLinear(c: number): number {
  return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
}

function linearToSrgb(c: number): number {
  return c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
}

export function hexToOklch(hex: string): Oklch {
  const m = hex.match(HEX_RE);
  if (!m) throw new Error(`invalid hex: ${hex}`);
  const body = m[1];
  const r = srgbToLinear(parseInt(body.slice(0, 2), 16) / 255);
  const g = srgbToLinear(parseInt(body.slice(2, 4), 16) / 255);
  const b = srgbToLinear(parseInt(body.slice(4, 6), 16) / 255);
  const a = body.length === 8 ? parseInt(body.slice(6, 8), 16) / 255 : 1;

  const lLin = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
  const mLin = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
  const sLin = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;

  const l_ = Math.cbrt(lLin);
  const m_ = Math.cbrt(mLin);
  const s_ = Math.cbrt(sLin);

  const L = 0.2104542553 * l_ + 0.793617785 * m_ - 0.0040720468 * s_;
  const aa = 1.9779984951 * l_ - 2.428592205 * m_ + 0.4505937099 * s_;
  const bb = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.808675766 * s_;

  const c = Math.hypot(aa, bb);
  let h = (Math.atan2(bb, aa) * 180) / Math.PI;
  if (h < 0) h += 360;
  return { l: L, c, h, a };
}

export function oklchToHex({ l, c, h, a }: Oklch): string {
  const hr = (h * Math.PI) / 180;
  const aa = c * Math.cos(hr);
  const bb = c * Math.sin(hr);

  const l_ = l + 0.3963377774 * aa + 0.2158037573 * bb;
  const m_ = l - 0.1055613458 * aa - 0.0638541728 * bb;
  const s_ = l - 0.0894841775 * aa - 1.291485548 * bb;

  const lLin = l_ * l_ * l_;
  const mLin = m_ * m_ * m_;
  const sLin = s_ * s_ * s_;

  const r = 4.0767416621 * lLin - 3.3077115913 * mLin + 0.2309699292 * sLin;
  const g = -1.2684380046 * lLin + 2.6097574011 * mLin - 0.3413193965 * sLin;
  const b = -0.0041960863 * lLin - 0.7034186147 * mLin + 1.707614701 * sLin;

  const toByte = (v: number): string => {
    const clamped = Math.max(0, Math.min(1, linearToSrgb(v)));
    return Math.round(clamped * 255)
      .toString(16)
      .padStart(2, "0");
  };
  const alphaByte = Math.round(Math.max(0, Math.min(1, a)) * 255)
    .toString(16)
    .padStart(2, "0");
  return `#${toByte(r)}${toByte(g)}${toByte(b)}${alphaByte}`;
}

/** Serialize to a CSS `oklch(L C H / A)` string. L/A are clamped to [0,1] and
 *  chroma to >= 0; chroma has NO upper clamp, so out-of-sRGB chroma is kept
 *  exactly (the browser maps it at paint). Rounded to the picker's slider
 *  precision so slider values round-trip exactly. */
export function formatOklch({ l, c, h, a }: Oklch): string {
  const round = (x: number, d: number) => {
    const f = 10 ** d;
    return Math.round((Number.isFinite(x) ? x : 0) * f) / f;
  };
  const L = round(Math.min(1, Math.max(0, l)), 4);
  const C = round(Math.max(0, c), 4);
  const H = round(((h % 360) + 360) % 360, 2);
  const A = round(Math.min(1, Math.max(0, a)), 3);
  return `oklch(${L} ${C} ${H} / ${A})`;
}

/** Parse a stored color to Oklch. Accepts our `oklch(L C H / A)` strings (the
 *  canonical form) and legacy/pasted hex. Lightness/alpha may be written as a
 *  percentage; missing alpha defaults to 1. */
export function parseColor(value: string): Oklch {
  const v = value.trim();
  if (v.toLowerCase().startsWith("oklch(")) {
    const inner = v.slice(v.indexOf("(") + 1, v.lastIndexOf(")"));
    const [coords, alphaStr] = inner.split("/");
    const parts = coords
      .trim()
      .split(/[\s,]+/)
      .filter(Boolean);
    const pct = (s: string): number => {
      const n = parseFloat(s);
      return s.trim().endsWith("%") ? n / 100 : n;
    };
    const fin = (n: number, fallback: number) => (Number.isFinite(n) ? n : fallback);
    return {
      l: fin(parts[0] != null ? pct(parts[0]) : NaN, 0),
      c: fin(parts[1] != null ? parseFloat(parts[1]) : NaN, 0),
      h: fin(parts[2] != null ? parseFloat(parts[2]) : NaN, 0),
      a: fin(alphaStr != null ? pct(alphaStr) : NaN, 1),
    };
  }
  return hexToOklch(v);
}

/** True for a string we can store: our `oklch(...)` form or a hex. */
export function isValidColor(s: string): boolean {
  const v = s.trim();
  if (HEX_RE.test(v)) return true;
  return /^oklch\(\s*[\d.]+%?\s+[\d.]+%?\s/i.test(v) && v.endsWith(")");
}

// Control points of the neutral surface stepping curve: light-mode OKLCH
// lightness -> dark-mode lightness, lifted straight from the `--default-*` /
// `--default-d-*` ladders in app.css (KEEP IN SYNC), extended with (0->1) and
// (1->0) so pure black and white invert exactly. Strictly monotonic, so the
// map is a bijection and `lightFromDark(darkFromLight(L)) === L` exactly: the
// inversion is perfectly reversible, only chroma/hue/alpha pass through. This
// is why a single shared transform can drive both the picker round-trip and the
// rendered `--*-dark` variables without drift.
const STEP_LIGHT_L = [0, 0.205, 0.269, 0.371, 0.439, 0.556, 0.708, 0.871, 0.922, 0.97, 0.985, 1];
const STEP_DARK_L = [1, 0.97, 0.922, 0.871, 0.79, 0.708, 0.556, 0.42, 0.28, 0.245, 0.205, 0];

/** Piecewise-linear interpolate `x` against `keys` (strictly monotonic, either
 *  direction) onto the matching `vals`; clamps outside the key range. */
function interp(x: number, keys: number[], vals: number[]): number {
  for (let i = 0; i < keys.length - 1; i++) {
    const lo = Math.min(keys[i], keys[i + 1]);
    const hi = Math.max(keys[i], keys[i + 1]);
    if (x >= lo && x <= hi) {
      const t = keys[i + 1] === keys[i] ? 0 : (x - keys[i]) / (keys[i + 1] - keys[i]);
      return vals[i] + t * (vals[i + 1] - vals[i]);
    }
  }
  const ascending = keys[keys.length - 1] > keys[0];
  const belowFirst = ascending ? x < keys[0] : x > keys[0];
  return belowFirst ? vals[0] : vals[vals.length - 1];
}

/** Return `value` with its OKLCH lightness replaced by `l` (chroma/hue/alpha
 *  kept). Used to normalize "seed" colors to a fixed mid lightness, since the
 *  theme ladders derive each shade's lightness and ignore the seed's own. */
export function withLightness(value: string, l: number): string {
  return formatOklch({ ...parseColor(value), l });
}

/** Theme inversion: the dark-mode rendering of a stored light-mode color,
 *  mapping lightness along the surface stepping curve (chroma/hue/alpha kept
 *  exactly -- no gamut clamp). */
export function darkFromLight(value: string): string {
  const o = parseColor(value);
  return formatOklch({ ...o, l: interp(o.l, STEP_LIGHT_L, STEP_DARK_L) });
}

/** Inverse of `darkFromLight` (back-compute the stored light value when the
 *  user edits a color in dark mode). */
export function lightFromDark(value: string): string {
  const o = parseColor(value);
  return formatOklch({ ...o, l: interp(o.l, STEP_DARK_L, STEP_LIGHT_L) });
}

/** The displayed (current-theme) form of a stored color field value: seed colors
 *  (with a `lockedLightness`) are pinned to that lightness, others render as
 *  stored, then theme-inverted in dark mode. Single source for both the swatch
 *  preview and the picker's initial color, so the client and website agree. */
export function storedToDisplay(stored: string, isDark: boolean, lockedLightness?: number): string {
  const lightForm = lockedLightness != null ? withLightness(stored, lockedLightness) : stored;
  return isDark ? darkFromLight(lightForm) : formatOklch(parseColor(lightForm));
}

/** Inverse of `storedToDisplay`: convert a displayed (current-theme) color back
 *  to the stored light-mode form, pinning seed colors to their locked lightness. */
export function displayToStored(
  displayed: string,
  isDark: boolean,
  lockedLightness?: number,
): string {
  const next = formatOklch(parseColor(displayed));
  const asLight = isDark ? lightFromDark(next) : next;
  return lockedLightness != null ? withLightness(asLight, lockedLightness) : asLight;
}

/** True when a color has any alpha at all. Per-component theme overrides use a
 *  fully-transparent value (alpha 0) to mean "inherit the global default"; any
 *  other alpha means the override is active. */
export function hasAlpha(value: string | undefined | null): boolean {
  if (typeof value !== "string" || value.length === 0) return false;
  try {
    return parseColor(value).a > 0;
  } catch {
    return false;
  }
}
