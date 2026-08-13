import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type UserMessageView from "../components/chat/messages/UserMessageView.svelte";

// Sample prop bundles for UserMessageView, consumed by the gallery and the
// website chat demos. Snippet props (editBody/attachmentRow) are supplied by
// the renderer; these carry the data/callback props, type-checked against the
// View so a prop rename fails svelte-check.
export const userMessageSamples = {
  default: { text: "How do I install Tomat on macOS?" },
  multiline: {
    text: "Summarize this article.\n\nThen list three follow-up questions I should ask.",
  },
  editing: { text: "draft message being edited", editable: true, active: true },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof UserMessageView>>>;
