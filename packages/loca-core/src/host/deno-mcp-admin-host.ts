// DenoHost MCP administration: adapts the engine's McpAdminHost onto the Deno
// service's MCP subsystem - the DB-backed registry, the live connection manager
// (stdio + remote), and the loopback OAuth authorization-code flow. The engine's
// /api/v1/mcp routes orchestrate through this; the subsystem itself stays in the
// shell (stdio servers spawn subprocesses, OAuth runs a loopback listener).

import type { McpAdminHost } from "@loca/core-engine";
import type { McpServer, McpServerInput } from "@loca/shared";
import { mcpRegistry } from "../mcp/registry.ts";
import { mcpManager } from "../mcp/manager.ts";
import { flattenPromptMessages } from "../mcp/tokens.ts";
import { cancel as cancelMcpOAuth, startMcpOAuth } from "../mcp/oauth-flow.ts";
import { mcpAuthSecretName, mcpOAuthSecretName } from "../mcp/secret-key.ts";

export const denoMcpAdminHost: McpAdminHost = {
  list: () => mcpRegistry().list(),
  listPrompts: () => mcpRegistry().listPrompts(),
  listResources: () => mcpRegistry().listResources(),
  get: (id: string): McpServer => mcpRegistry().getOrThrow(id),
  create: (input: McpServerInput): McpServer => mcpRegistry().create(input),
  update: (id: string, patch: Partial<McpServerInput>): McpServer =>
    mcpRegistry().update(id, patch),
  delete: (id: string): void => mcpRegistry().delete(id),
  setToolEnabled: (id: string, tool: string, enabled: boolean): McpServer =>
    mcpRegistry().setToolEnabled(id, tool, enabled),
  setPromptEnabled: (id: string, prompt: string, enabled: boolean): McpServer =>
    mcpRegistry().setPromptEnabled(id, prompt, enabled),
  setToolAlwaysAvailable: (id: string, tool: string, alwaysAvailable: boolean): McpServer =>
    mcpRegistry().setToolAlwaysAvailable(id, tool, alwaysAvailable),
  resolvePrompt: async (
    id: string,
    prompt: string,
    args: Record<string, string>,
  ): Promise<string> => {
    const got = await mcpManager().getPrompt(id, prompt, args);
    return flattenPromptMessages(got.messages);
  },
  // Reconcile connections to match the current enabled set.
  resync: (): Promise<void> => mcpManager().sync(mcpRegistry().list()),
  disconnect: (id: string): Promise<void> => mcpManager().disconnect(id),
  startOAuth: (id: string, url: string, onComplete: (ok: boolean) => void) =>
    startMcpOAuth(id, url, onComplete),
  cancelOAuth: (id: string): void => cancelMcpOAuth(id),
  authSecretName: (id: string): string => mcpAuthSecretName(id),
  oauthSecretName: (id: string): string => mcpOAuthSecretName(id),
};
