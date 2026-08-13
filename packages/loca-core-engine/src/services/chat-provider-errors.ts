// Maps a provider/SDK error to a stable ErrorCode + message. The OpenAI SDK
// throws APIError with `status` + `code` and human-readable messages; try the
// structured fields first, then fall back to message regex.

import type { ErrorCode } from "@loca/shared";
import { errMessage } from "@loca/shared";

export function classifyProviderError(err: unknown): { code: ErrorCode; message: string } {
  const msg = errMessage(err);
  const status =
    (err as { status?: number; statusCode?: number } | null)?.status ??
    (err as { statusCode?: number } | null)?.statusCode;
  const code =
    (err as { code?: string; error?: { code?: string } } | null)?.code ??
    (err as { error?: { code?: string } } | null)?.error?.code;

  if (status === 401 || code === "invalid_api_key") {
    return { code: "provider_unauthorized", message: msg };
  }
  if (status === 429 || code === "rate_limit_exceeded") {
    return { code: "provider_rate_limited", message: msg };
  }
  if (status === 503 || status === 504) {
    return { code: "server_unavailable", message: msg };
  }
  if (
    code === "context_length_exceeded" ||
    /context length|maximum context length|context window/i.test(msg)
  ) {
    return { code: "context_window_exceeded", message: msg };
  }
  return { code: "provider_error", message: msg };
}
