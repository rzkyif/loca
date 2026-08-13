// DenoHost ToolHost: wires the engine's tool catalog + execution surface onto the
// Deno service's extension registry, worker sandbox, and MCP client. This is the
// one place the engine's chat graph reaches those subsystems; a mobile host would
// supply a remote-MCP-only ToolHost instead. The worker CallEvent / CallController
// are structurally identical to the engine's ToolCallEvent / ToolCallController,
// so startToolCall passes them straight through.

import type { ToolCallController, ToolCallEvent, ToolCallSpec, ToolHost } from "@loca/core-engine";
import type { Tool } from "@loca/shared";
import { extensionsRegistry } from "../extensions/registry.ts";
import { validateAndNormalizeToolArgs } from "../extensions/validate-args.ts";
import { workerPool } from "../extensions/worker-pool.ts";
import { mcpRegistry } from "../mcp/registry.ts";
import { mcpManager } from "../mcp/manager.ts";
import { mcpResolveTokens } from "../mcp/tokens.ts";

export const denoToolHost: ToolHost = {
  installedExtensionTools(): Tool[] {
    const out: Tool[] = [];
    for (const ext of extensionsRegistry().list()) {
      if (ext.status !== "installed") continue;
      for (const tool of extensionsRegistry().listTools(ext.id)) out.push(tool);
    }
    return out;
  },
  getTool(toolId: string): Tool | undefined {
    return extensionsRegistry().getTool(toolId);
  },
  listMcpTools(): Tool[] {
    return mcpRegistry().listAllTools();
  },
  isMcpServer(id: string): boolean {
    return mcpRegistry().get(id) !== undefined;
  },
  loadToolEmbeddings(): Map<string, { vector: Float32Array; sourceHash: string }> {
    return extensionsRegistry().loadAllEmbeddings();
  },
  verifyToolFresh(extensionId: string): Promise<void> {
    return extensionsRegistry().verifyHashFresh(extensionId);
  },
  validateToolArgs(tool: Tool, argumentsJson: string): string {
    return validateAndNormalizeToolArgs(tool, argumentsJson);
  },
  startToolCall(spec: ToolCallSpec, onEvent: (event: ToolCallEvent) => void): ToolCallController {
    return workerPool().startCall(spec, onEvent);
  },
  callMcpTool(
    serverId: string,
    name: string,
    args: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<unknown> {
    return mcpManager().callTool(serverId, name, args, signal);
  },
  resolveMcpTokens(text: string): Promise<{ block: string | null; claimed: Set<string> }> {
    return mcpResolveTokens(text);
  },
};
