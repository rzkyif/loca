// AppError: thrown from anywhere in the engine, serialized to the wire format by
// the HTTP error middleware.

import { ERROR_STATUS, type ErrorCode } from "@loca/shared";

export class AppError extends Error {
  readonly code: ErrorCode;
  readonly status: number;
  readonly details?: Record<string, unknown>;

  constructor(code: ErrorCode, message: string, details?: Record<string, unknown>) {
    super(message);
    this.name = "AppError";
    this.code = code;
    this.status = ERROR_STATUS[code];
    this.details = details;
  }
}

export function isAppError(value: unknown): value is AppError {
  return value instanceof AppError;
}

// Helpers for the common error-throwing one-liners.
export function notFound(message: string, details?: Record<string, unknown>): never {
  throw new AppError("not_found", message, details);
}

export function validation(message: string, details?: Record<string, unknown>): never {
  throw new AppError("validation_error", message, details);
}

export function conflict(message: string, details?: Record<string, unknown>): never {
  throw new AppError("conflict", message, details);
}

export function forbidden(message: string, details?: Record<string, unknown>): never {
  throw new AppError("forbidden", message, details);
}

export function internal(message: string, details?: Record<string, unknown>): never {
  throw new AppError("internal_error", message, details);
}

// True for filesystem errors caused by a full disk, surfaced as a generic OS
// error on most platforms; matched on the underlying errno/code (`ENOSPC`) AND
// the human-readable message because the error shape isn't fully typed across
// platforms. Used by the HTTP error middleware to translate disk-full into a 507.
export function isNoSpaceError(err: unknown): boolean {
  if (!(err instanceof Error)) return false;
  const code =
    (err as unknown as { code?: string }).code ?? (err as unknown as { errno?: string }).errno;
  if (code === "ENOSPC" || code === "ERROR_DISK_FULL") return true;
  const msg = err.message.toLowerCase();
  return (
    msg.includes("no space left") || msg.includes("disk full") || msg.includes("quota exceeded")
  );
}
