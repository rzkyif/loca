// Display sample: push one of every one-way display kind (markdown, image,
// table, diff) into the chat. These are fire-and-forget bubbles; nothing
// flows back to the tool.

import type { ToolContext } from "./types.ts";
import { SAMPLE_PNG_B64, SAMPLE_PNG_MIME } from "./sample-data.ts";

export function sampleDisplay(
  _args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ pushed: number }> {
  ctx.display.markdown("# Sample heading\n\nThis bubble is **markdown**.");
  ctx.display.image(SAMPLE_PNG_B64, SAMPLE_PNG_MIME, "A 1x1 sample image");
  ctx.display.table(
    ["item", "amount"],
    [
      ["Apples", "3"],
      ["Bananas", "6"],
    ],
  );
  ctx.display.diff("before text", "after text", "sample.txt");
  return Promise.resolve({ pushed: 4 });
}
