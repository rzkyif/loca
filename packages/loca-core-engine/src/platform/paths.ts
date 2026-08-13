// The portable subset of the on-disk layout, derived from `host().rootDir`. Only
// the paths the engine's own services need live here (the DB, settings, the
// secrets blob, session + memory dirs, per-extension data). Ports, the binary
// dir, the shared models dir, and staging stay in core's paths.ts because they
// are sidecar / subprocess / download concerns the engine never touches.
//
// These MUST resolve to the same absolute paths core's paths.ts produces (the
// Deno host sets rootDir = coreRoot()), so a service reading through enginePaths
// hits the exact files core wrote before the move.

import { join } from "@std/path";
import { host } from "./runtime.ts";

export interface EnginePaths {
  root: string;
  configFile: string;
  settingsFile: string;
  secretsEncFile: string;
  dbFile: string;
  sessionsDir: string;
  memoriesDir: string;
}

export function enginePaths(): EnginePaths {
  const root = host().rootDir;
  return {
    root,
    configFile: join(root, "core.json"),
    settingsFile: join(root, "settings.json"),
    secretsEncFile: join(root, "secrets.enc"),
    dbFile: join(root, "core.sqlite"),
    sessionsDir: join(root, "sessions"),
    memoriesDir: join(root, "memories"),
  };
}

export function sessionDir(sessionId: string): string {
  return join(enginePaths().sessionsDir, sessionId);
}

export function sessionAttachmentsDir(sessionId: string): string {
  return join(sessionDir(sessionId), "attachments");
}

/** Per-extension private data dir (the module broker's `db` SQLite lives here),
 *  separate from the install dir so reinstalls never touch user data. */
export function extensionDataDir(extensionId: string): string {
  return join(enginePaths().root, "extension-data", extensionId);
}
