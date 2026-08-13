import type { Alignment } from "./types.ts";

// When two stacked bubbles merge into one (the CoreBar under the SessionBar in
// chat, or under the Settings panel), the seam between them is dissolved: the
// narrower bubble squares off the corners facing the wider one so it reads as
// plugging into it, the lower bubble is pulled up so their paddings overlap with
// no gap, and that lower bubble drops its shadow/halo on the merged edge. The
// client wrappers compute a `BubbleMerge` for each side and feed it to the
// shared Views; a floating (unmerged) bubble gets `NO_MERGE`.

/** The four bubble corners, `<vertical><horizontal>`. */
export type BubbleCorner = "tl" | "tr" | "bl" | "br";

export interface BubbleMerge {
  /** Corners to square off (radius 0) so this bubble butts flush against its
   *  merge neighbor. */
  flatCorners: BubbleCorner[];
  /** This is the LOWER bubble of the merged pair: pull it up by one padding onto
   *  the bubble above (removing the gap) and clip its top shadow/halo, so the two
   *  read as a single bubble. */
  overlapTop: boolean;
}

export const NO_MERGE: BubbleMerge = { flatCorners: [], overlapTop: false };

/** The corners a bubble squares off on its merge `edge`. The narrower bubble (the
 *  "plug") flattens BOTH corners on that edge; the wider bubble keeps its rounding
 *  except, when the UI is edge-anchored, the one corner on the aligned side so the
 *  two bubbles share a continuous straight edge there (a centered pair leaves the
 *  wider bubble fully rounded). */
export function mergeFlatCorners(
  align: Alignment,
  edge: "top" | "bottom",
  narrow: boolean,
): BubbleCorner[] {
  const [start, end]: BubbleCorner[] = edge === "top" ? ["tl", "tr"] : ["bl", "br"];
  if (narrow) return [start, end];
  if (align === "left") return [start];
  if (align === "right") return [end];
  return [];
}
