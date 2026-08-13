// Wire-format error envelope and the enumerated error codes core can return.
// Every non-2xx response uses { error: { code, message, details? } }.

export const ERROR_CODES = [
  // Auth / pairing
  "invalid_token",
  "revoked",
  "missing_token",
  "admin_token_required",
  "admin_password_required",
  "admin_password_invalid",
  "admin_password_rate_limited",
  "invalid_pairing_code",
  "pairing_code_expired",
  "pairing_code_claimed",
  "pairing_rate_limited",

  // Generic resource
  "not_found",
  "forbidden",
  "validation_error",
  "conflict",

  // Sessions / chat
  "session_busy",
  "session_not_found",
  "message_not_found",
  "stream_not_found",
  "context_window_exceeded",

  // LLM / scheduling
  "server_busy",
  "server_unavailable",
  "provider_error",
  "provider_unauthorized",
  "provider_rate_limited",

  // Extensions
  "no_loca_json",
  "invalid_loca_json",
  "deps_install_failed",
  "tarball_fetch_failed",
  "extract_failed",
  "extension_not_found",
  "tool_not_found",
  "permissions_required",
  "permissions_revoked",
  "extension_hash_drift",
  "extension_already_installed",
  "tool_name_collision",

  // Models / binaries
  "model_not_found",
  "binary_not_found",
  "checksum_mismatch",
  "signature_invalid",
  "manifest_fetch_failed",

  // Update
  "update_failed",
  "manual_rollback_required",
  "update_in_progress",

  // Storage
  "insufficient_storage",

  // Internal
  "internal_error",
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

export interface ApiErrorBody {
  error: {
    code: ErrorCode;
    message: string;
    details?: Record<string, unknown>;
  };
}

export function isApiErrorBody(value: unknown): value is ApiErrorBody {
  if (!value || typeof value !== "object") return false;
  const err = (value as { error?: unknown }).error;
  if (!err || typeof err !== "object") return false;
  const code = (err as { code?: unknown }).code;
  return typeof code === "string" && (ERROR_CODES as readonly string[]).includes(code);
}

/** Extract a human-readable message from an unknown thrown value. Replaces the
 *  `err instanceof Error ? err.message : String(err)` one-liner repeated across
 *  core and client. */
export function errMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

// HTTP status code recommended for each error code. Core's middleware reads
// this when serializing AppError to a Response.
export const ERROR_STATUS: Record<ErrorCode, number> = {
  invalid_token: 401,
  revoked: 401,
  missing_token: 401,
  admin_token_required: 401,
  admin_password_required: 401,
  admin_password_invalid: 401,
  admin_password_rate_limited: 429,
  invalid_pairing_code: 401,
  pairing_code_expired: 410,
  pairing_code_claimed: 410,
  pairing_rate_limited: 429,

  not_found: 404,
  forbidden: 403,
  validation_error: 400,
  conflict: 409,

  session_busy: 409,
  session_not_found: 404,
  message_not_found: 404,
  stream_not_found: 404,
  context_window_exceeded: 413,

  server_busy: 503,
  server_unavailable: 503,
  provider_error: 502,
  provider_unauthorized: 401,
  provider_rate_limited: 429,

  no_loca_json: 422,
  invalid_loca_json: 422,
  deps_install_failed: 500,
  tarball_fetch_failed: 502,
  extract_failed: 500,
  extension_not_found: 404,
  tool_not_found: 404,
  permissions_required: 412,
  permissions_revoked: 412,
  extension_hash_drift: 409,
  extension_already_installed: 409,
  tool_name_collision: 409,

  model_not_found: 404,
  binary_not_found: 404,
  checksum_mismatch: 422,
  signature_invalid: 422,
  manifest_fetch_failed: 502,

  update_failed: 500,
  manual_rollback_required: 500,
  update_in_progress: 409,

  insufficient_storage: 507,

  internal_error: 500,
};
