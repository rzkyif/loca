// @loca/core-engine: the runtime-agnostic heart of loca-core. It owns the
// portable session/chat/settings/memory logic and exposes it behind a Host
// abstraction, so the same source runs both inside the Deno service
// (`@loca/core`, which supplies a DenoHost) and, in a future pass, inside the
// mobile client's webview (a webview host). See ./host.ts for the contract and
// the package README for the boundary.

export { init } from "./engine.ts";
export type { EngineConnection, EngineInitOpts, EngineInstance } from "./engine.ts";
export type { ClientResolver, EngineClient } from "./http/middleware/auth.ts";
export { __resetFrameBusForTesting, FrameBus, frameBus } from "./frame-bus.ts";

// Platform layer: the runtime handle plus the portable utilities moved services
// import in place of core's Deno-coupled shared/*. The embedder calls attachHost
// once at boot; core re-exports the utils from their old paths (thin shims) so
// existing imports don't churn.
export { attachHost, host } from "./platform/runtime.ts";
export { getLogger, scrubSecrets } from "./platform/log.ts";
export type { Logger } from "./platform/log.ts";
export {
  enginePaths,
  extensionDataDir,
  sessionAttachmentsDir,
  sessionDir,
} from "./platform/paths.ts";
export type { EnginePaths } from "./platform/paths.ts";
export { sha256HexSync, toHex } from "./platform/hash.ts";
export { closeDb, db, openDb } from "./platform/db.ts";
export { isWithin } from "./platform/fs-safety.ts";
export {
  AppError,
  conflict,
  forbidden,
  internal,
  isAppError,
  isNoSpaceError,
  notFound,
  validation,
} from "./platform/errors.ts";
export {
  newAttachmentId,
  newCallId,
  newClientId,
  newJobId,
  newMcpServerId,
  newMemoryId,
  newMessageId,
  newRequestId,
  newScheduledPromptId,
  newSessionId,
  newStreamId,
} from "./platform/ids.ts";
export type {
  Host,
  HostCapabilities,
  HostDb,
  HostFs,
  HostSecureStore,
  HostStmt,
  LocalEndpoints,
  LogLevel,
  McpAdminHost,
  SqlBindValue,
  StatusHost,
  WriteOpts,
} from "./host.ts";
export type {
  ToolCallController,
  ToolCallEvent,
  ToolCallSpec,
  ToolHost,
} from "./services/tool-host.ts";
