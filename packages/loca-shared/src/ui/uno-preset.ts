import { presetIcons, presetTypography, presetWind4, type UserConfig } from "unocss";
import extractorSvelte from "@unocss/extractor-svelte";
import { lookupCollection } from "@iconify/json";
import { CSS_EASING, INTERACTIVE_MS } from "./animations.ts";

// The shared slice of the UnoCSS config that both the client (Vite) and the
// website (Astro) spread into their own `defineConfig`. It owns everything that
// must paint identically in both apps: the wind4 + icon + typography presets,
// the font theme variables (resolved from app.css / base.css), the Svelte class
// extractor, and the `*-default-*` / `*-accent-*-*` / `surface` shortcuts plus
// the `rounded-{small,medium,large}` rules that consume the CSS variables
// declared in styles/base.css. Each app keeps its OWN `content` pipeline (which
// dirs to scan), since that is build-tool specific; the website additionally
// scans this package's component dir so these classes are generated for the
// extracted components.
export function locaUnoBase(): UserConfig {
  return {
    presets: [
      presetWind4({ dark: "class" }),
      presetIcons({
        collections: {
          "material-symbols": () => lookupCollection("material-symbols"),
          "line-md": () => lookupCollection("line-md"),
        },
      }),
      presetTypography(),
    ],
    // Route font-sans through the runtime-overridable --font-default variable
    // (written by appearance.defaultFont in the client; static default on the
    // website via base.css). font-mono is deliberately NOT overridden here:
    // naming it would make wind4 emit `--font-mono: var(--font-mono)`, a
    // self-reference that, depending on stylesheet order, clobbers base.css's
    // real stack (it only survived in the client because JS sets --font-mono
    // inline). Leaving mono at the preset default keeps `.font-mono` resolving
    // to a real stack, and the client's inline --font-mono override still wins.
    theme: {
      font: {
        sans: "var(--font-default)",
      },
    },
    extractors: [extractorSvelte()],
    // Interactive-state variants that fire on real pointer input AND on a
    // `data-hover` / `data-active` attribute. The website's scripted demo cursor
    // sets those attributes on whatever it is "pointing at", so a demo and a real
    // user resolve the EXACT same styles (e.g. `hov:text-default-800`) with no
    // duplicated hover CSS. Interactive extracted components use `hov:`/`act:`
    // in place of `hover:`/`active:`.
    //
    // The real `:hover` rule is gated behind `@media (hover: hover)` so it only
    // fires on hover-capable pointers. On touch, `:hover` would otherwise stick
    // after a tap (the element stays "hovered" until the next tap elsewhere) and
    // would never fire at rest, so any "dim until hover" control reads as too
    // dim. The `[data-hover]` half stays unguarded so the scripted demo cursor
    // (and any deliberate attribute toggle) resolves the same styles on every
    // device. `act:`/`:active` works on touch as-is, so it needs no media gate.
    variants: [
      (matcher) =>
        matcher.startsWith("hov:")
          ? [
              {
                matcher: matcher.slice(4),
                selector: (s) => `${s}:hover`,
                parent: "@media (hover: hover)",
              },
              {
                matcher: matcher.slice(4),
                selector: (s) => `${s}[data-hover]`,
              },
            ]
          : undefined,
      (matcher) =>
        matcher.startsWith("act:")
          ? {
              matcher: matcher.slice(4),
              selector: (s) => `${s}:active, ${s}[data-active]`,
            }
          : undefined,
    ],
    shortcuts: [
      // The single canonical transition for button-like interaction feedback:
      // hover/press color shifts at INTERACTIVE_MS with the shared CSS_EASING.
      // Every clickable element uses this in place of a bare `transition-colors`
      // so hover and press feel identical app-wide. CSS_EASING's spaces are
      // stripped because UnoCSS arbitrary values treat spaces as token breaks.
      [
        "transition-interactive",
        `transition-colors duration-[${INTERACTIVE_MS}ms] ease-[${CSS_EASING.replace(/ /g, "")}]`,
      ],
      // The themable color tokens. Each `*-default-*` and `*-accent-*-*` class
      // resolves to a CSS variable defined in base.css; those variables are
      // derived from a single user-customizable base hex per scope via
      // `oklch(from var(--<scope>-base) <L> c h / alpha)`. See base.css for the
      // light/dark L tables and the per-scope variable declarations.
      [/^bg-default-(\d+)$/, ([, s]) => `bg-[var(--default-${s})] dark:bg-[var(--default-d-${s})]`],
      [
        /^text-default-(\d+)$/,
        ([, s]) => `text-[var(--default-${s})] dark:text-[var(--default-d-${s})]`,
      ],
      [
        /^border-default-(\d+)$/,
        ([, s]) => `border-[var(--default-${s})] dark:border-[var(--default-d-${s})]`,
      ],
      [
        /^bg-default-inverted-(\d+)$/,
        ([, s]) => `bg-[var(--default-d-${s})] dark:bg-[var(--default-${s})]`,
      ],
      [
        /^text-default-inverted-(\d+)$/,
        ([, s]) => `text-[var(--default-d-${s})] dark:text-[var(--default-${s})]`,
      ],
      [
        /^border-default-inverted-(\d+)$/,
        ([, s]) => `border-[var(--default-d-${s})] dark:border-[var(--default-${s})]`,
      ],
      [
        /^from-default-(\d+)$/,
        ([, s]) => `from-[var(--default-${s})] dark:from-[var(--default-d-${s})]`,
      ],
      [/^to-default-(\d+)$/, ([, s]) => `to-[var(--default-${s})] dark:to-[var(--default-d-${s})]`],
      [
        /^via-default-(\d+)$/,
        ([, s]) => `via-[var(--default-${s})] dark:via-[var(--default-d-${s})]`,
      ],
      [
        /^bg-accent-(blue|purple|red|green|yellow)-(\d+)$/,
        ([, c, s]) => `bg-[var(--accent-${c}-${s})] dark:bg-[var(--accent-${c}-d-${s})]`,
      ],
      [
        /^text-accent-(blue|purple|red|green|yellow)-(\d+)$/,
        ([, c, s]) => `text-[var(--accent-${c}-${s})] dark:text-[var(--accent-${c}-d-${s})]`,
      ],
      [
        /^border-accent-(blue|purple|red|green|yellow)-(\d+)$/,
        ([, c, s]) => `border-[var(--accent-${c}-${s})] dark:border-[var(--accent-${c}-d-${s})]`,
      ],
      [
        /^outline-accent-(blue|purple|red|green|yellow)-(\d+)$/,
        ([, c, s]) => `outline-[var(--accent-${c}-${s})] dark:outline-[var(--accent-${c}-d-${s})]`,
      ],
      // Semantic surface tokens: the single source of truth for what shade a
      // panel vs an on-panel well/control paints. Each expands to a `*-default-N`
      // shortcut above, so dark-mode inversion and per-component `--default-base`
      // theming carry through for free. The flat surface model is one `surface`
      // for every panel/card/bubble/modal/popover, one `inset` for wells and
      // filled controls, and a deeper `inset-strong` reserved for the few
      // multi-shade controls (toggles, segmented control, a neutral chip sitting
      // on an inset row). Pure indicators (slider track/thumb, segmented
      // selected fill) intentionally keep raw `bg-default-400/500`.
      ["bg-surface", "bg-default-50"],
      ["bg-surface-inset", "bg-default-200"],
      ["bg-surface-inset-strong", "bg-default-300"],
      ["border-surface", "border-default-200"],
    ],
    rules: [
      // Three customizable roundedness buckets driven by
      // `--rounded-{small,medium,large}` CSS variables (defaults set in
      // base.css, runtime values applied in the client from the
      // appearance.rounded* settings). The class names are intentionally
      // unique (small/medium/large rather than md/lg/xl) so they don't
      // collide with UnoCSS's preset rounded utilities. Other rounded
      // utilities (rounded-sm, rounded-full, etc.) are left to the preset.
      [/^rounded-small$/, () => ({ "border-radius": "var(--rounded-small)" })],
      [/^rounded-medium$/, () => ({ "border-radius": "var(--rounded-medium)" })],
      [/^rounded-large$/, () => ({ "border-radius": "var(--rounded-large)" })],
      [
        /^rounded-l-small$/,
        () => ({
          "border-top-left-radius": "var(--rounded-small)",
          "border-bottom-left-radius": "var(--rounded-small)",
        }),
      ],
      [
        /^rounded-r-small$/,
        () => ({
          "border-top-right-radius": "var(--rounded-small)",
          "border-bottom-right-radius": "var(--rounded-small)",
        }),
      ],
    ],
  };
}
