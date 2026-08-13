import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ColorPickerModalView from "../components/settings/ColorPickerModalView.svelte";

export const colorPickerModalSamples = {
  opaque: {
    open: true,
    l: 0.62,
    c: 0.21,
    h: 28,
    a: 1,
    newColor: "oklch(0.62 0.21 28)",
    previousColor: "oklch(0.55 0.18 264)",
  },
  withAlpha: {
    open: true,
    l: 0.7,
    c: 0.15,
    h: 200,
    a: 0.45,
    newColor: "oklch(0.7 0.15 200 / 0.45)",
    previousColor: "oklch(0.7 0.15 200 / 1)",
  },
  seedLocked: {
    open: true,
    lockLightness: true,
    l: 0.62,
    c: 0.18,
    h: 145,
    a: 1,
    newColor: "oklch(0.62 0.18 145)",
    previousColor: "oklch(0.62 0.2 95)",
  },
  noPrevious: {
    open: true,
    l: 0.5,
    c: 0.12,
    h: 320,
    a: 1,
    newColor: "oklch(0.5 0.12 320)",
    previousColor: null,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ColorPickerModalView>>>;
