import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type DiffView from "../components/chat/messages/DiffView.svelte";

export const diffViewSamples = {
  default: {
    before: "const port = 7800;\nstart(port);\n",
    after: "const port = 7800;\nlog(`starting on ${port}`);\nstart(port);\n",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof DiffView>>>;
