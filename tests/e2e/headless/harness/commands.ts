// vitest browser-mode custom commands (run in Node) that let an in-browser test
// drive host-side actions: spawn/stop a real core, script the mock LLM, and
// inspect the core's filesystem for assertions. Registered in vitest.config.ts
// under `test.browser.commands`; called from the browser via
// `import { commands } from "vitest/browser"`.

import { existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { startCore, type CoreInstance, type ScenarioOptions } from "./core-process.ts";
import type { LlmScript, RecordedLlmRequest } from "./mock-services.ts";

const cores = new Map<string, CoreInstance>();

export interface LaunchResult {
  id: string;
  baseUrl: string;
  adminToken: string;
  mockBaseUrl: string;
  tlsPin: string;
}

export async function launchCore(_ctx: unknown, opts: ScenarioOptions = {}): Promise<LaunchResult> {
  const core = await startCore(opts);
  cores.set(core.id, core);
  return {
    id: core.id,
    baseUrl: core.baseUrl,
    adminToken: core.adminToken,
    mockBaseUrl: core.mock.baseUrl,
    tlsPin: core.tlsPin,
  };
}

export async function stopCore(_ctx: unknown, id: string): Promise<void> {
  const core = cores.get(id);
  if (!core) return;
  cores.delete(id);
  await core.stop();
}

export function setLlmScript(_ctx: unknown, id: string, script: LlmScript): void {
  cores.get(id)?.mock.setLlmScript(script);
}

export function getLlmRequests(_ctx: unknown, id: string): RecordedLlmRequest[] {
  return cores.get(id)?.mock.requests() ?? [];
}

/** The core's recent stderr lines, for failure diagnostics. */
export function getCoreLogs(_ctx: unknown, id: string): string[] {
  return cores.get(id)?.recentLogs() ?? [];
}

/** Kill the core subprocess but keep its home + port (a network/process blip a
 *  reconnect spec can recover from with bringCoreBack). */
export async function killCore(_ctx: unknown, id: string): Promise<void> {
  await cores.get(id)?.kill();
}

/** Respawn a killed core on the same home + port (same TLS pin). */
export async function bringCoreBack(_ctx: unknown, id: string): Promise<void> {
  await cores.get(id)?.restart();
}

/** Does a path (relative to the core's LOCA_CORE_HOME) exist? Used to assert a
 *  download landed. Note the shared models dir resolves to <home>/models. */
export function coreFileExists(_ctx: unknown, id: string, relPath: string): boolean {
  const core = cores.get(id);
  if (!core) return false;
  return existsSync(join(core.home, relPath));
}

/** Absolute path to a fixture directory under fixtures/ (e.g. a test extension
 *  the core copies in on local install). */
export function fixturePath(_ctx: unknown, name: string): string {
  return fileURLToPath(new URL(`../fixtures/${name}`, import.meta.url));
}

export async function stopAllCores(_ctx: unknown): Promise<void> {
  const all = [...cores.values()];
  cores.clear();
  await Promise.all(all.map((c) => c.stop()));
}

// The map vitest registers. Keys become `commands.<name>` in the browser.
export const e2eCommands = {
  launchCore,
  stopCore,
  setLlmScript,
  getLlmRequests,
  getCoreLogs,
  killCore,
  bringCoreBack,
  coreFileExists,
  fixturePath,
  stopAllCores,
};
