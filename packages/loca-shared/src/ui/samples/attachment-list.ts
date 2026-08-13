import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type AttachmentListView from "../components/chat/AttachmentListView.svelte";

// A 1x1 transparent PNG, so the gallery's image thumbnails resolve without any
// platform image loading (the client passes resolved data URLs in `imageUrls`).
const BLANK_PNG =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

export const attachmentListSamples = {
  documents: {
    parts: [
      { type: "document", filename: "report.pdf", markdown: "# Report" },
      { type: "document_file", filename: "notes.md", path: "/tmp/notes.md" },
    ],
  },
  withImage: {
    parts: [
      {
        type: "image_file",
        filename: "screenshot.png",
        path: "/tmp/screenshot.png",
        mime: "image/png",
      },
      { type: "document", filename: "report.pdf", markdown: "# Report" },
    ],
    imageUrls: { "/tmp/screenshot.png": BLANK_PNG },
  },
  editable: {
    editable: true,
    onRemove: () => {},
    parts: [
      {
        type: "image_file",
        filename: "screenshot.png",
        path: "/tmp/screenshot.png",
        mime: "image/png",
      },
      { type: "document", filename: "report.pdf", markdown: "# Report" },
    ],
    imageUrls: { "/tmp/screenshot.png": BLANK_PNG },
  },
  imageUrl: {
    parts: [{ type: "image_url", image_url: { url: BLANK_PNG } }],
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof AttachmentListView>>>;
