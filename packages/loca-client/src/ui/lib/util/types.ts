/**
 * Shared TypeScript types used across the app: messages, attachments,
 * tool calls, server status, and session info. The shapes here are the
 * common vocabulary between the UI, the state stores, and the sidecar
 * layer.
 */

// Re-export canonical definitions from the shared package so client and core
// agree on the wire shape. `path` on `*_file` parts is the core REST URL
// (/api/v1/sessions/:sid/attachments/:aid) returned by the upload endpoint;
// the server parses the trailing :aid to load bytes when building the LLM
// request. The import + re-export form is needed because plain
// `export type {} from "..."` doesn't bring the name into the local scope
// for use in interfaces below.
import type {
  AskUserAnswer,
  AskUserChoiceQuestion,
  AskUserQuestion,
  DisplayContent,
  MemoryFilterEntryPersisted,
  MessageContent,
  MessagePart,
  PermissionKind,
  ToolCall,
  ToolCallStatus,
  ToolFilterEntryPersisted,
  ToolFilterPhase1Persisted,
} from "@loca/shared";
export type {
  AskUserAnswer,
  AskUserChoiceQuestion,
  AskUserQuestion,
  DisplayContent,
  MessageContent,
  MessagePart,
  ToolCallStatus,
};

export type ToolCallAskUserState = {
  requestId: string;
  questions: AskUserQuestion[];
  answers: AskUserAnswer[] | null;
};

/** A runtime permission request raised by a running tool (Deno prompt
 *  paused on the worker's PTY). Mirrors the `tool.permission_request` WS
 *  frame; the decision UI lives in UserInput's permission mode. */
export type ToolCallPermissionState = {
  requestId: string;
  permissionKind: PermissionKind;
  resource: string;
  apiName?: string;
  declared: boolean;
  reason?: string;
};

export type ToolCallLogLine = {
  level: "debug" | "info" | "warn" | "error";
  message: string;
  ts: number;
};

/** Never-persisted UI overlay on a `role: "tool"` message: the live log
 *  lines and the in-flight askUser form state the tool.* frames deliver.
 *  Kept outside the wire fields so a server snapshot replace can never
 *  wipe it (applyServerMessage merges it back). */
export type UiEphemera = {
  logs?: ToolCallLogLine[];
  askUser?: ToolCallAskUserState;
  permissionRequest?: ToolCallPermissionState;
};

/** Client-side message. One bag type holding the union of every wire role's
 *  fields (see `Message` in @loca/shared) plus the client-only `loading`
 *  role and the `ephemera` overlay. Server `chat.message` snapshots are
 *  applied directly, so live streaming and a session reload share one shape
 *  by construction; components narrow by `role`. */
export type Message = {
  /** Server-minted for every chat-born message; client-minted only for
   *  user messages, the system bubble, and the loading sentinel. */
  id?: string;
  ord?: number;
  createdAtMs?: number;
  role:
    | "user"
    | "assistant"
    | "reasoning"
    | "error"
    | "system"
    | "tool"
    | "tool_filter"
    | "memory_filter"
    /** Tool-pushed display content (ctx.display.* / show_memory). */
    | "display"
    /** Synthetic, render-only role. The +page rendering pipeline injects a
     *  single `role: "loading"` Message into its derived display list while
     *  awaiting the first response chunk so a transient spinner bubble flows
     *  through the same small-bubble stacking layout as adjacent reasoning /
     *  tool_filter / system bubbles. Never persisted, never added to
     *  messagesState.messages, never sent to the LLM. */
    | "loading";
  /** Absent on the wire for tool / tool_filter rows. `role: "display"`
   *  rows carry a DisplayContent payload here (the wire field is also
   *  `content`); every other role carries MessageContent. Narrow with
   *  asMessageContent / displayContentOf. */
  content?: MessageContent | DisplayContent;
  /** Only populated on user messages. True when core authored the message
   *  itself (a scheduled prompt or greeting); rendered as a collapsed
   *  "Automated Prompt" bubble. */
  automated?: boolean;
  modelUsed?: "default" | "secondary";
  /** Set when the stream was aborted (user interrupt or provider error)
   *  before the model finished; content is the partial text. */
  interrupted?: boolean;
  /** Set when the model hit the context window (finish_reason "length")
   *  instead of stopping naturally; content may be partial or empty. */
  truncated?: boolean;
  /** Only populated on user messages. Holds the resolved system prompt that
   *  was sent to the LLM for that turn, including any snippet-triggered
   *  transformations. Used by sendMessages() on edit-and-resend. */
  systemPromptOverride?: string;
  /** Only populated on `role: "reasoning"` messages. Elapsed time (ms) from
   *  the first reasoning chunk to the first content chunk (or stream finish
   *  if no content). */
  reasoningDurationMs?: number;
  /** Only populated on `role: "reasoning"` messages. Points back at the
   *  assistant content message produced in the same turn. */
  pairedAssistantId?: string;
  /** Assistant transcript-replay data: the tool calls the model emitted. */
  toolCalls?: ToolCall[];
  // role: "tool" flat fields (ToolMessage on the wire).
  callId?: string;
  extensionId?: string;
  toolName?: string;
  /** JSON string of arguments as the model emitted them. */
  arguments?: string;
  /** ToolCallStatus for tool rows; "complete" | "error" for tool_filter /
   *  memory_filter rows. */
  status?: ToolCallStatus | "complete" | "error";
  progress?: number;
  label?: string;
  description?: string;
  result?: unknown;
  error?: string;
  // role: "tool_filter" flat fields (ToolFilterMessage on the wire). An
  // absent phase means it didn't run; an empty array means it ran and
  // produced nothing.
  phase1?: ToolFilterPhase1Persisted[];
  phase2?: ToolFilterEntryPersisted[];
  alwaysAvailable?: ToolFilterEntryPersisted[];
  nameMatched?: ToolFilterEntryPersisted[];
  mcp?: ToolFilterEntryPersisted[];
  toolsSent?: number;
  errorMessage?: string;
  // role: "memory_filter" flat fields (MemoryFilterMessage on the wire).
  // Empty array means the filter ran and surfaced nothing.
  relevant?: MemoryFilterEntryPersisted[];
  // role: "error" extras.
  code?: string;
  details?: Record<string, unknown>;
  /** Never-persisted UI overlay (tool logs, askUser form state). */
  ephemera?: UiEphemera;
};

/** Generate a message id. User message ids are set separately from their
 *  attachment timestamp (see `addUserMessage`) so this only needs to be
 *  unique, not a bare timestamp. A process-local counter guards against
 *  same-millisecond collisions between assistant / system / tool messages
 *  minted back-to-back in a single event loop turn. */
let messageIdCounter = 0;
export function makeMessageId(): string {
  return `${Date.now()}-${(++messageIdCounter).toString(36)}`;
}
export type Monitor = { id: string | number; name: string; isPrimary: boolean };
export type Alignment = "left" | "center" | "right";

export type DownloadStatus = "Pending" | "Downloading" | "Completed" | "Error" | "Cancelled";
export type DownloadDestination = "Models";

// Wire-format DownloadEntry (camelCase per the new shared API). Keeping
// the old `DownloadItem` name + snake_case aliases as getters would be
// a migration crust we explicitly avoid; UI sites that read these fields
// switch to camelCase as part of the rework.
export interface DownloadItem {
  id: string;
  source: string;
  destination: "models" | "binaries" | "extensions";
  relPath: string;
  absPath: string;
  filename: string;
  groupId: string;
  sizeBytes?: number;
  downloadedBytes: number;
  status: DownloadStatus;
  error?: string;
  addedAtMs: number;
}

export type LLMErrorType =
  | "rate_limit_error"
  | "context_length_exceeded_error"
  | "authentication_error"
  | "invalid_request_error"
  | "server_error"
  | "unknown_error";

// Local re-export of the server-defined wire shape so the rest of the
// client can keep importing `TokenUsage` from `$lib/util/types`. The
// canonical definition lives in @loca/shared/domain/session.ts.
import type { TokenUsage as SharedTokenUsage } from "@loca/shared";
export type TokenUsage = SharedTokenUsage;

export type SessionInfo = {
  id: string;
  title: string;
};

/** Wire shape returned by the server when re-hydrating a session. */
export interface ChatHistoryPayload {
  sessionId: string;
  title: string;
  contextUsage: TokenUsage | null;
  messages: Message[];
}

export type Attachment = {
  type: "image" | "document";
  filename: string;
  // Base64 (images) or markdown (documents). Lives in memory until the
  // containing message is sent and flushed to disk.
  pendingData: string;
  mime?: string;
};

/** Extract plain text from MessageContent, regardless of format. Display
 *  payloads carry no chat text and yield "". */
export function getTextContent(content: Message["content"]): string {
  if (content === undefined || content === null) return "";
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((p): p is Extract<MessagePart, { type: "text" }> => p.type === "text")
    .map((p) => p.text)
    .join("\n");
}

/** Narrow a bag content value to MessageContent ("" for display payloads). */
export function asMessageContent(content: Message["content"]): MessageContent {
  if (content === undefined || content === null) return "";
  if (typeof content === "string" || Array.isArray(content)) return content;
  return "";
}

/** The DisplayContent of a `role: "display"` row, null otherwise. */
export function displayContentOf(msg: Message): DisplayContent | null {
  if (msg.role !== "display") return null;
  const c = msg.content;
  if (c && typeof c === "object" && !Array.isArray(c) && "type" in c) return c;
  return null;
}
