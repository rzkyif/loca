import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type SnippetAutocompleteView from "../components/chat/SnippetAutocompleteView.svelte";

const noop = (): void => {};

export const snippetAutocompleteSamples = {
  default: {
    options: [
      { id: "1", name: "Greeting", trigger: "@hi" },
      { id: "2", name: "Signature", trigger: "@sig" },
      { id: "3", name: "Install guide", trigger: "@install" },
    ],
    selectedIndex: 0,
    anchor: { top: 0, left: 0 },
    onSelect: noop,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof SnippetAutocompleteView>>>;
