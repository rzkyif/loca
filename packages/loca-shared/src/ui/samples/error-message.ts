import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ErrorMessageView from "../components/chat/messages/ErrorMessageView.svelte";

export const errorMessageSamples = {
  rateLimit: { errorType: "rate_limit_error" },
  withDetail: {
    errorType: "server_error",
    errorDetail: "HTTP 503\nupstream connect error or disconnect/reset before headers",
  },
  unknown: { errorType: "unknown_error" },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ErrorMessageView>>>;
