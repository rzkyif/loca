import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type MessageStackView from "../components/chat/MessageStackView.svelte";

// Scripted stand-ins for the horizontal substack chrome. The client owns the
// live messages and drives the fade widths from runtime rect measurement; these
// scenarios fix the fade state directly so the gallery shows each edge case.
// The `bubble` snippet (the live message bubble) is supplied by the renderer.

export const messageStackSamples = {
  // A single bubble: nothing hidden, both edge fades collapsed.
  single: {
    count: 1,
    alignment: "left",
    fadeLeft: "0px",
    fadeRight: "0px",
  },

  // A left-aligned row overflowing past its right edge: the right fade shows.
  overflowing: {
    count: 6,
    alignment: "left",
    fadeLeft: "0px",
    fadeRight: "1rem",
  },

  // Scrolled into the middle of a right-aligned (flex-row-reverse) row: both
  // fades show because content is hidden on either side.
  rightScrolled: {
    count: 6,
    alignment: "right",
    fadeLeft: "1rem",
    fadeRight: "1rem",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof MessageStackView>>>;
