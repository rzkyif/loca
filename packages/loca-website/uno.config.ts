import { defineConfig } from "unocss";
// Relative path (not the @loca/shared alias): uno.config.ts is loaded by
// UnoCSS via jiti, outside Vite/Astro resolution, so the alias is unavailable
// here. Same shared preset the client spreads, for an identical design system.
import { locaUnoBase } from "../loca-shared/src/ui/uno-preset.ts";

// Theme-flipped client renditions. Every client surface the website renders (the
// homepage showcase stages, the inline manual demos, and the gallery cards)
// carries `.demo-frame`, and renders in the OPPOSITE theme of the surrounding
// site: a dark site shows light renditions, a light site shows dark ones. The
// theme is decided by which CSS-variable ladder the shared components read
// (`--*-N` light / `--*-d-N` dark, picked by the `.dark` ancestor on <html>). We
// can't strip that ancestor for a subtree, so instead we SWAP the two ladders'
// values inside the frame: each light slot takes the dark value and vice versa.
// Because BOTH slots swap, `*-inverted-*` tokens (which read the opposite ladder:
// the Send button, toggles, selected option cards) stay correct too, and the
// swap is unconditional so a live theme toggle just re-selects the (already
// inverted) ladder with no observer.
//
// Two flip strategies, chosen by how each token resolves:
//   - The `--accent-*` shades and the bubble seed pairs resolve from a :root-only
//     seed, so the :root block captures each slot's live value under a neutral
//     `--flip-*` name and the frame block assigns every slot its sibling's
//     capture. No lightness constants for these, so base.css changes flow through.
//   - The `--default-*` ladder is per element: base.css derives it from
//     `--default-base` on `*`, so a subtree that locally retints (an accent bubble
//     sets `--default-base` to `var(--accent-*-base)`) must keep deriving from
//     THAT base while flipped. A frozen :root capture would pin every demo
//     descendant to the global neutral hue and drop the accent (accent bubbles
//     would render gray). So this ladder is re-derived straight from the in-scope
//     `--default-base`, swapping the light and dark lightness. With the global
//     neutral base that is identical to the old capture; an accent subtree now
//     flips while keeping its hue. These lightnesses are the one place constants
//     are mirrored from base.css's `*` block -- keep them in sync.
//
// A `.demo-unflip` block reverses the swap for a subtree, restoring each slot to
// its website-theme value (the capture for the :root-seeded pairs, the in-scope
// re-derivation for the default ladder). The manual demo frames use it on the
// inner content wrapper so the FRAME chrome and dot grid render flipped while the
// client component inside follows the website theme (a "double flip"). It rides a
// selector of equal specificity to the flip but is emitted after it, so it wins
// for the nested wrapper and its descendants.
function themeFlipCss(): string {
  const shades = [50, 100, 200, 300, 400, 500, 600, 700, 800, 900];
  const accents = ["red", "blue", "purple", "green", "yellow"];
  // :root-seeded pairs (accent shades + bubble seeds): captured once and swapped.
  const pairs: Array<[string, string]> = [];
  for (const c of accents) {
    for (const s of shades) {
      pairs.push([`--accent-${c}-${s}`, `--accent-${c}-d-${s}`]);
    }
  }
  for (const b of ["user", "agent", "agent2"]) {
    pairs.push([`--${b}-bubble-bg-light`, `--${b}-bubble-bg-dark`]);
    pairs.push([`--${b}-bubble-border-light`, `--${b}-bubble-border-dark`]);
    pairs.push([`--${b}-bubble-text-light`, `--${b}-bubble-text-dark`]);
  }
  pairs.push(["--bubble-bg-l-light", "--bubble-bg-l-dark"]);
  pairs.push(["--bubble-shadow-color-light", "--bubble-shadow-color-dark"]);
  pairs.push(["--bubble-flat-shadow-light", "--bubble-flat-shadow-dark"]);

  const tmp = (v: string) => `--flip-${v.slice(2)}`;
  const capture = pairs
    .flatMap(([l, d]) => [`  ${tmp(l)}: var(${l});`, `  ${tmp(d)}: var(${d});`])
    .join("\n");
  const swap = pairs
    .flatMap(([l, d]) => [`  ${l}: var(${tmp(d)});`, `  ${d}: var(${tmp(l)});`])
    .join("\n");
  // Restore: each slot back to its own captured value, undoing any ancestor flip.
  const restore = pairs
    .flatMap(([l, d]) => [`  ${l}: var(${tmp(l)});`, `  ${d}: var(${tmp(d)});`])
    .join("\n");

  // The `--default-*` ladder re-derives from the in-scope `--default-base` (see
  // the header): swap = light slot takes the dark lightness and vice versa;
  // unflip restores each slot's own lightness. Lightnesses MIRROR base.css's `*`
  // block -- keep them in sync. Riding `.demo-frame *` outranks base.css's `*`.
  const lightL: Record<number, number> = {
    50: 0.985,
    100: 0.97,
    200: 0.922,
    300: 0.871,
    400: 0.708,
    500: 0.556,
    600: 0.439,
    700: 0.371,
    800: 0.269,
    900: 0.205,
  };
  const darkL: Record<number, number> = {
    50: 0.205,
    100: 0.245,
    200: 0.28,
    300: 0.42,
    400: 0.556,
    500: 0.708,
    600: 0.79,
    700: 0.871,
    800: 0.922,
    900: 0.97,
  };
  const der = (l: number) => `oklch(from var(--default-base) ${l} c h / alpha)`;
  const defaultSwap = shades
    .flatMap((s) => [
      `  --default-${s}: ${der(darkL[s])};`,
      `  --default-d-${s}: ${der(lightL[s])};`,
    ])
    .join("\n");
  const defaultRestore = shades
    .flatMap((s) => [
      `  --default-${s}: ${der(lightL[s])};`,
      `  --default-d-${s}: ${der(darkL[s])};`,
    ])
    .join("\n");

  return [
    `:root {\n${capture}\n}`,
    `.demo-frame,\n.demo-frame * {\n${swap}\n${defaultSwap}\n}`,
    `.demo-unflip,\n.demo-unflip * {\n${restore}\n${defaultRestore}\n}`,
  ].join("\n");
}

export default defineConfig({
  ...locaUnoBase(),
  preflights: [{ getCSS: themeFlipCss }],
  content: {
    // Scan the source from disk up front, so every utility any page uses is in
    // the single global stylesheet from the first load. Without this, dev only
    // generates utilities for files Vite has already transformed (imported), so
    // a class unique to a not-yet-visited page (e.g. a demo's `p-6` or the
    // settings panel's fixed size) is absent right after a view-transition
    // soft-navigation and the demo renders unstyled until a reload. Filesystem
    // scanning makes the stylesheet complete and stable across navigations.
    filesystem: ["src/**/*.{astro,svelte,ts,mdx,html}", "../loca-shared/src/ui/**/*.svelte"],
    pipeline: {
      include: [
        // This site's own source.
        /\.(astro|svelte|[jt]sx?|mdx?|html)($|\?)/,
        // The extracted shared components, so their utility classes are
        // generated in the website build too.
        /loca-shared\/src\/ui\/.*\.svelte($|\?)/,
      ],
    },
  },
});
