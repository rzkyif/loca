// Translates thrown errors into the wire-format error envelope. Wired into
// the engine app via `app.onError()` in app.ts. That's the only Hono 4 hook
// that catches throws from sub-apps mounted via `app.route()`. Core re-exports
// `sendError` from its old `http/middleware/errors.ts` path so shell routes are
// unchanged.

import type { Context } from "hono";
import { errMessage } from "@loca/shared";
import type { ContentfulStatusCode } from "hono/utils/http-status";
import type { ApiErrorBody, ErrorCode } from "@loca/shared";
import { AppError, isAppError, isNoSpaceError } from "../../platform/errors.ts";
import { getLogger, scrubSecrets } from "../../platform/log.ts";

const log = getLogger("http");

export function sendError(c: Context, err: unknown): Response {
  if (isAppError(err)) {
    log.warn(`${c.req.method} ${c.req.path} -> ${err.code}: ${err.message}`);
    return c.json(toBody(err.code, err.message, err.details), err.status as ContentfulStatusCode);
  }
  if (isNoSpaceError(err)) {
    const message = (err as Error).message;
    log.warn(`${c.req.method} ${c.req.path} -> insufficient_storage: ${message}`);
    return c.json(toBody("insufficient_storage", message), 507);
  }
  const message = errMessage(err);
  log.error(`${c.req.method} ${c.req.path} -> internal_error: ${message}`);
  return c.json(toBody("internal_error", message), 500);
}

function toBody(code: ErrorCode, message: string, details?: Record<string, unknown>): ApiErrorBody {
  // Scrub before the message crosses the wire. Logs are already scrubbed by the
  // logger; response bodies are not, and an internal_error can carry a raw
  // exception string (e.g. an upstream provider error echoing a URL+token).
  return {
    error: {
      code,
      message: scrubSecrets(message),
      ...(details ? { details } : {}),
    },
  };
}

export { AppError };
