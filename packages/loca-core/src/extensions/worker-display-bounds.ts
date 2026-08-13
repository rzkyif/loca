// Bounds for tool display payloads: accepted content is persisted into the
// session file (rewritten on every later message) and broadcast to every
// client, so oversize markdown is truncated and the kinds that cannot be
// truncated without changing their meaning are dropped.

import type { DisplayContent } from "@loca/shared";

const DISPLAY_TEXT_MAX_CHARS = 256_000;
const DISPLAY_IMAGE_MAX_B64_CHARS = 8_000_000;
const DISPLAY_TABLE_MAX_ROWS = 1_000;
const DISPLAY_TABLE_MAX_COLUMNS = 64;
const DISPLAY_TABLE_MAX_CELL_CHARS = 4_096;

export function boundDisplayContent(
  content: DisplayContent,
): { content: DisplayContent } | { error: string } {
  switch (content.type) {
    case "markdown":
      if (content.markdown.length > DISPLAY_TEXT_MAX_CHARS) {
        return {
          content: {
            type: "markdown",
            markdown: content.markdown.slice(0, DISPLAY_TEXT_MAX_CHARS) + "\n\n[truncated]",
          },
        };
      }
      return { content };
    case "image":
      if (content.dataB64.length > DISPLAY_IMAGE_MAX_B64_CHARS) {
        return {
          error: `image exceeds ${DISPLAY_IMAGE_MAX_B64_CHARS} base64 characters`,
        };
      }
      return { content };
    case "table": {
      if (content.columns.length > DISPLAY_TABLE_MAX_COLUMNS) {
        return { error: `table exceeds ${DISPLAY_TABLE_MAX_COLUMNS} columns` };
      }
      if (content.rows.length > DISPLAY_TABLE_MAX_ROWS) {
        return { error: `table exceeds ${DISPLAY_TABLE_MAX_ROWS} rows` };
      }
      const oversizeCell =
        content.columns.some((c) => c.length > DISPLAY_TABLE_MAX_CELL_CHARS) ||
        content.rows.some((r) => r.some((c) => c.length > DISPLAY_TABLE_MAX_CELL_CHARS));
      if (oversizeCell) {
        return {
          error: `table cell exceeds ${DISPLAY_TABLE_MAX_CELL_CHARS} characters`,
        };
      }
      return { content };
    }
    case "diff":
      if (
        content.before.length > DISPLAY_TEXT_MAX_CHARS ||
        content.after.length > DISPLAY_TEXT_MAX_CHARS
      ) {
        return {
          error: `diff side exceeds ${DISPLAY_TEXT_MAX_CHARS} characters`,
        };
      }
      return { content };
  }
}
