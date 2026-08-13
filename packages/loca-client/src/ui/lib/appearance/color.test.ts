// hex <-> OKLCH round-trip + light/dark variant math. Pure number
// crunching; no DOM, no Tauri.

import { describe, expect, it } from "vitest";
import {
  darkFromLight,
  formatOklch,
  hasAlpha,
  hexToOklch,
  isValidHex,
  lightFromDark,
  oklchToHex,
  parseColor,
  toEightCharHex,
  withLightness,
} from "./color";

describe("isValidHex", () => {
  it("accepts 6-char and 8-char hex", () => {
    expect(isValidHex("#abcdef")).toBe(true);
    expect(isValidHex("#ABCDEF")).toBe(true);
    expect(isValidHex("#abcdef12")).toBe(true);
  });
  it("rejects other lengths and non-hex characters", () => {
    expect(isValidHex("#abc")).toBe(false);
    expect(isValidHex("abcdef")).toBe(false);
    expect(isValidHex("#abcdeg")).toBe(false);
    expect(isValidHex("#abcdef1")).toBe(false);
  });
});

describe("toEightCharHex", () => {
  it("pads 6-char hex with ff alpha", () => {
    expect(toEightCharHex("#aabbcc")).toBe("#aabbccff");
  });
  it("lowercases 8-char hex", () => {
    expect(toEightCharHex("#AABBCC11")).toBe("#aabbcc11");
  });
  it("throws on invalid hex", () => {
    expect(() => toEightCharHex("not-a-hex")).toThrow(/invalid hex/);
  });
});

describe("hexToOklch + oklchToHex", () => {
  it("round-trips primary colors within 1 byte per channel", () => {
    for (const hex of ["#000000ff", "#ffffffff", "#ff0000ff", "#00ff00ff", "#0000ffff"]) {
      const ok = hexToOklch(hex);
      const back = oklchToHex(ok);
      // Each channel within tolerance of the original.
      for (let i = 1; i < hex.length; i += 2) {
        const orig = parseInt(hex.slice(i, i + 2), 16);
        const round = parseInt(back.slice(i, i + 2), 16);
        expect(Math.abs(orig - round)).toBeLessThanOrEqual(1);
      }
    }
  });

  it("preserves alpha through the round-trip", () => {
    const hex = "#aabbcc44";
    const ok = hexToOklch(hex);
    expect(ok.a).toBeCloseTo(0x44 / 255, 4);
    const back = oklchToHex(ok);
    expect(back.slice(7, 9)).toBe("44");
  });
});

describe("parseColor / formatOklch", () => {
  it("round-trips an oklch string exactly", () => {
    const s = "oklch(0.7 0.4 0 / 1)";
    expect(formatOklch(parseColor(s))).toBe(s);
  });
  it("preserves out-of-sRGB chroma (no gamut clamp)", () => {
    // 0.4 chroma at this hue/lightness is outside sRGB; storing it must keep it.
    expect(parseColor("oklch(0.7 0.4 0 / 1)").c).toBe(0.4);
  });
  it("parses legacy hex", () => {
    const o = parseColor("#000000ff");
    expect(o.l).toBeCloseTo(0, 3);
    expect(o.a).toBe(1);
  });
  it("withLightness keeps chroma/hue/alpha, swaps lightness", () => {
    const out = parseColor(withLightness("oklch(0.3 0.12 250 / 0.5)", 0.7));
    expect(out.l).toBeCloseTo(0.7, 4);
    expect(out.c).toBeCloseTo(0.12, 4);
    expect(out.h).toBeCloseTo(250, 2);
    expect(out.a).toBeCloseTo(0.5, 3);
  });
});

describe("darkFromLight / lightFromDark", () => {
  it("inverse functions cancel in OKLCH (lightness round-trips; hue/chroma/alpha kept)", () => {
    const start = parseColor("#5577aa");
    const back = parseColor(lightFromDark(darkFromLight("#5577aa")));
    expect(back.l).toBeCloseTo(start.l, 3);
    expect(back.c).toBeCloseTo(start.c, 3);
    expect(back.h).toBeCloseTo(start.h, 1);
    expect(back.a).toBeCloseTo(start.a, 3);
  });
  it("returns oklch() strings", () => {
    expect(darkFromLight("#5577aa").startsWith("oklch(")).toBe(true);
  });
});

describe("hasAlpha", () => {
  it("is true for 7-char hex (no alpha byte, defaults to opaque)", () => {
    expect(hasAlpha("#aabbcc")).toBe(true);
  });
  it("is false for 8-char hex with #..00 (sentinel for inherit)", () => {
    expect(hasAlpha("#aabbcc00")).toBe(false);
  });
  it("is true for 8-char hex with any non-zero alpha", () => {
    expect(hasAlpha("#aabbcc01")).toBe(true);
    expect(hasAlpha("#aabbccff")).toBe(true);
  });
  it("is false for missing / non-string input and wrong-length strings", () => {
    // `hasAlpha` is permissive about content: it only inspects length
    // and the trailing two characters. We cover the explicit non-string
    // and the length-mismatch branches; 7- or 9-char strings are
    // treated as a 7-char hex (opaque) or 9-char hex (alpha-bearing)
    // by design.
    expect(hasAlpha(undefined)).toBe(false);
    expect(hasAlpha(null)).toBe(false);
    // length 10: neither 7 nor 9 → false branch.
    expect(hasAlpha("0123456789")).toBe(false);
  });
});
