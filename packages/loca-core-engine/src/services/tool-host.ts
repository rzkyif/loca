// ToolHost: the unified tool catalog + execution surface the engine's chat graph
// reaches instead of importing core's extension registry, MCP client, and worker
// sandbox directly. The Deno service supplies a full implementation
// (DenoToolHost, wiring extensionsRegistry + workerPool + mcpRegistry +
// mcpManager); a mobile host supplies a reduced one (remote-MCP tools only, with
// the extension-exec methods throwing / the catalog returning no extension
// tools). Reached through host().tools.
//
// The protocol types below are structurally identical to core's worker-call
// CallEvent / CallController, so the DenoToolHost adapter passes them through
// without translation while the engine references only these engine-owned types.

import type {
  AskUserAnswer,
  AskUserQuestion,
  DisplayContent,
  PermissionKind,
  ScheduledPromptDraft,
  Tool,
} from "@loca/shared";

// One event emitted by a running extension tool call. The chat orchestrator maps
// each to a ServerToClientFrame (progress / askUser / permission / schedule / log
// / display / cancelled).
export type ToolCallEvent =
  | { kind: "progress"; progress: number; label?: string; description?: string }
  | { kind: "ask_user_request"; requestId: string; questions: AskUserQuestion[] }
  | {
      kind: "permission_request";
      requestId: string;
      permission: PermissionKind;
      resource: string;
      apiName?: string;
      declared: boolean;
      reason?: string;
    }
  | { kind: "schedule_request"; requestId: string; draft: ScheduledPromptDraft }
  | { kind: "log"; level: "debug" | "info" | "warn" | "error"; message: string }
  | { kind: "stderr_log"; line: string }
  | { kind: "display"; content: DisplayContent }
  | { kind: "tool_cancelled" };

// A handle on one in-flight extension tool call: feed prompt responses back and
// await the result. `done` resolves with the tool_result value; it rejects
// (AppError) on tool_error / cancel / timeout / worker exit.
export interface ToolCallController {
  callId: string;
  cancel(): void;
  respondAskUser(requestId: string, answers: AskUserAnswer[]): void;
  respondPermission(requestId: string, allow: boolean): void;
  respondSchedule(requestId: string, accepted: boolean, draft?: ScheduledPromptDraft): void;
  hasPendingSchedule(requestId: string): boolean;
  done: Promise<unknown>;
}

// The spec for starting one extension tool call in the sandbox.
export interface ToolCallSpec {
  extensionId: string;
  tool: Tool;
  argumentsJson: string;
  chatContext: { userMessage: string; sessionId: string | null; locale?: string };
}

export interface ToolHost {
  // --- catalog ---
  // Every tool of an installed extension (enabled AND disabled; grants attached;
  // OS-unsupported tools already filtered out). The engine applies its own
  // enabled / exposable gating on top.
  installedExtensionTools(): Tool[];
  // A single tool by `${extensionId}::${name}`, or undefined if absent /
  // OS-unsupported.
  getTool(toolId: string): Tool | undefined;
  // Every enabled MCP server's tools, in the shared Tool shape.
  listMcpTools(): Tool[];
  // True when `id` is a connected MCP server (so a call routes to the MCP client
  // rather than the worker sandbox).
  isMcpServer(id: string): boolean;
  // The persisted relevance vectors, keyed by tool id, for phase-1 cosine.
  loadToolEmbeddings(): Map<string, { vector: Float32Array; sourceHash: string }>;

  // --- execution ---
  // Re-verify an extension's content hash before a call (throws on drift).
  verifyToolFresh(extensionId: string): Promise<void>;
  // Validate + normalize model-emitted arguments against the tool schema.
  validateToolArgs(tool: Tool, argumentsJson: string): string;
  // Start an extension tool call in the sandbox; events stream to `onEvent`.
  startToolCall(spec: ToolCallSpec, onEvent: (event: ToolCallEvent) => void): ToolCallController;
  // Call an MCP tool on its server (a straight request/response).
  callMcpTool(
    serverId: string,
    name: string,
    args: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<unknown>;

  // --- tokens ---
  // Resolve @resource / /prompt tokens in the latest user message into injected
  // reference / instruction blocks.
  resolveMcpTokens(text: string): Promise<{ block: string | null; claimed: Set<string> }>;
}
