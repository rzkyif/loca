import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type CommandPreviewFieldView from "../components/settings/CommandPreviewFieldView.svelte";

// The client resolves the live command text from the current settings (async);
// these are scripted stand-ins covering a short command, a long one that scrolls
// horizontally, and the pre-resolution loading placeholder.
export const commandPreviewFieldSamples = {
  short: { preview: "loca-core serve --port 8787" },
  long: {
    preview: "deno run -A --unstable-kv jsr:@loca/core --channel stable --models ~/.loca/models",
  },
  loading: { preview: "Loading..." },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof CommandPreviewFieldView>>>;
