// The DenoHost: the @loca/core-engine Host implemented on the Deno runtime.
// It bundles the SQLite adapter, the async filesystem, the secure store, env
// config, logging, and the capability flags (a full Deno service can spawn
// sidecars and subprocesses). Core services reach it through denoHost() during
// the extraction; once they move into the engine, main.ts passes this same Host
// to engine.init().

import type { Host, HostCapabilities, LogLevel } from "@loca/core-engine";
import { coreRoot } from "../paths.ts";
import { getLogger } from "../shared/log.ts";
import { openDenoDb } from "../db/deno-sqlite.ts";
import { denoFs } from "./deno-fs.ts";
import { denoSecureStore } from "./deno-secure-store.ts";
import { denoLocalEndpoints } from "./deno-local-endpoints.ts";
import { denoToolHost } from "./deno-tool-host.ts";
import { denoStatusHost } from "./deno-status-host.ts";
import { denoMcpAdminHost } from "./deno-mcp-admin-host.ts";

// A full Deno service: it can run local inference sidecars and spawn
// subprocesses, and it reaches remote MCP servers.
const DESKTOP_CAPABILITIES: HostCapabilities = {
  localInference: true,
  subprocess: true,
  remoteMcp: true,
};

const impl: Host = {
  // A getter so a test that swaps LOCA_CORE_HOME between runs sees the new root
  // without rebuilding the host.
  get rootDir(): string {
    return coreRoot();
  },
  config(key: string): string | undefined {
    return Deno.env.get(key);
  },
  capabilities: DESKTOP_CAPABILITIES,
  fs: denoFs,
  openDb: openDenoDb,
  secureStore: denoSecureStore,
  localEndpoints: denoLocalEndpoints,
  tools: denoToolHost,
  status: denoStatusHost,
  mcp: denoMcpAdminHost,
  log(level: LogLevel, scope: string, message: string): void {
    getLogger(scope)[level](message);
  },
  now(): number {
    return Date.now();
  },
};

/** The process-wide DenoHost. Stateless (paths are read fresh per call), so a
 *  single shared instance is safe across the test env swaps that repoint
 *  LOCA_CORE_HOME. */
export function denoHost(): Host {
  return impl;
}
