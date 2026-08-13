// Session, Message, and tool-call state types shared between core and client.
// Message persistence lives core-side; the client renders these shapes.

export type MessageRole =
  | "user"
  | "assistant"
  | "system"
  | "tool"
  | "reasoning"
  | "tool_filter"
  | "memory_filter"
  | "display"
  | "error";

export interface AttachmentRef {
  id: string;
  filename: string;
  mime?: string;
  sizeBytes: number;
  // Core-relative URL the client uses to fetch bytes via the authed REST API.
  url: string;
}

// Multipart message content. A user message with no attachments is a plain
// `string`; once any attachment or inline image lands on the message, content
// becomes a `MessagePart[]` so each piece (text, image, document) can be
// rendered + serialized independently. `path` on `*_file` parts is the
// core-relative `/api/v1/sessions/:sid/attachments/:aid` URL produced by
// `uploadAttachment`. The server parses the trailing `:aid` to load bytes
// from disk when building the LLM request.
export type MessagePart =
  | { type: "text"; text: string }
  | { type: "image_url"; image_url: { url: string } }
  | { type: "image_file"; filename: string; path: string; mime: string }
  | { type: "document"; filename: string; markdown: string }
  | { type: "document_file"; filename: string; path: string };

export type MessageContent = string | MessagePart[];

/** True when content is a multipart array. */
export function isMultipart(c: MessageContent): c is MessagePart[] {
  return Array.isArray(c);
}

/** Flatten multipart content to a plain text representation (text parts
 *  joined; non-text parts dropped). Used by title generation, last-user-text
 *  lookups, and any code that wants a string regardless of attachments. */
export function contentToText(c: MessageContent): string {
  if (typeof c === "string") return c;
  return c
    .filter((p): p is Extract<MessagePart, { type: "text" }> => p.type === "text")
    .map((p) => p.text)
    .join("");
}

export interface TokenUsage {
  prompt: number;
  completion: number;
  total: number;
}

export type ToolCallStatus =
  | "pending"
  | "running"
  | "awaiting_user"
  | "awaiting_permission"
  | "completed"
  | "failed"
  | "cancelled";

export interface ToolCall {
  callId: string;
  extensionId: string;
  toolName: string;
  arguments: string; // JSON string the LLM emitted
  status: ToolCallStatus;
  result?: unknown; // present when status === "completed"
  error?: string; // present when status === "failed"
  /** 0..1 completion fraction set by the `tool.progress` WS frame. The
   *  frame also carries transient `label` / `description` strings; only the
   *  number is persisted on the ToolCall. */
  progress?: number;
  logLines?: Array<{
    level: "debug" | "info" | "warn" | "error";
    message: string;
    atMs: number;
  }>;
}

// One question in a tool's askUser request. Discriminated by `kind`;
// frames that predate the discriminator carry no kind and mean "choice".
// Per-kind answer shapes (one entry per question in the response frame):
// choice = chosen value (string[] when multiselect), diff = "accept" |
// "reject", files = chosen path(s), image = chosen action value, table =
// the edited rows as column-keyed records.
export interface AskUserChoiceQuestion {
  kind?: "choice";
  question: string;
  options?: Array<{ label: string; value: string; description?: string }>;
  multiselect?: boolean;
  allowFreeformInput?: boolean;
}

export interface AskUserDiffQuestion {
  kind: "diff";
  question: string;
  before: string;
  after: string;
  title?: string;
}

export interface AskUserFilesQuestion {
  kind: "files";
  question: string;
  entries: Array<{ path: string; label?: string; description?: string }>;
  multiselect?: boolean;
}

export interface AskUserImageQuestion {
  kind: "image";
  question: string;
  dataB64: string;
  mime: string;
  actions: Array<{ label: string; value: string }>;
}

export interface AskUserTableQuestion {
  kind: "table";
  question: string;
  columns: string[];
  rows: string[][];
}

export type AskUserQuestion =
  | AskUserChoiceQuestion
  | AskUserDiffQuestion
  | AskUserFilesQuestion
  | AskUserImageQuestion
  | AskUserTableQuestion;

export type AskUserAnswer = string | string[] | Array<Record<string, string>>;

// Base fields every message carries. Concrete shapes are role-specific below.
interface MessageBase {
  id: string;
  ord: number;
  createdAtMs: number;
}

export interface UserMessage extends MessageBase {
  role: "user";
  // Plain string when there are no attachments. Once an image/document is
  // attached the client switches to multipart so each piece can be rendered
  // and sent to the LLM independently (vision models expect `image_url`
  // parts; documents are inlined as text with a "[Attached document: …]"
  // prefix server-side).
  content: MessageContent;
  // Resolved system prompt that was sent to the LLM for THIS turn,
  // including any snippet-triggered transformations. Populated when a
  // snippet overrode the user's default system prompt; absent otherwise.
  // Used by edit-and-resend to replay the same prompt context.
  systemPromptOverride?: string;
  // True when core authored this message itself (a scheduled prompt or
  // greeting), not the user. The client renders it as a collapsed
  // "Automated Prompt" bubble instead of a user bubble.
  automated?: boolean;
}

export interface AssistantMessage extends MessageBase {
  role: "assistant";
  content: string;
  toolCalls?: ToolCall[];
  // Which model role produced this turn: "default" for the primary local /
  // external endpoint, "secondary" when dual-model routing kicked in.
  // Drives the model-name chip the UI renders next to the bubble.
  modelUsed?: "default" | "secondary";
  // True when the stream was aborted (user interrupt or provider error)
  // before the model finished; the persisted content is the partial text
  // streamed up to that point.
  interrupted?: boolean;
  // True when the model stopped because it hit the context window
  // (finish_reason "length"), not a natural stop. The reply may be partial or
  // empty (all the room went to thinking); the UI renders a "cut off" note.
  truncated?: boolean;
}

export interface SystemMessage extends MessageBase {
  role: "system";
  content: string;
}

export interface ToolMessage extends MessageBase {
  role: "tool";
  callId: string;
  extensionId: string;
  toolName: string;
  /** JSON string of arguments as the model emitted them. Persisted so the
   *  reloaded bubble keeps the call's inputs. */
  arguments: string;
  result?: unknown;
  error?: string;
  status: ToolCallStatus;
  /** 0..1 completion fraction from the last `tool.progress` event. */
  progress?: number;
  /** Last `tool.progress` label/description the tool emitted, persisted so
   *  the reloaded bubble keeps the tool's own wording (e.g. "Opening
   *  YouTube") instead of falling back to the generic tool-name phrase. */
  label?: string;
  description?: string;
}

export interface ReasoningMessage extends MessageBase {
  role: "reasoning";
  content: string;
  // True when the stream was aborted before the model finished thinking.
  interrupted?: boolean;
  // ms between the first reasoning chunk and the first content chunk (or
  // stream end if the model never emitted content). Lets the UI render
  // "Thought for Xs" without recomputing from timestamps.
  reasoningDurationMs?: number;
  // ID of the AssistantMessage produced in the same turn. Used by the UI
  // to pair the bubbles and by edit-and-resend to delete both atomically.
  pairedAssistantId?: string;
  // Which model role produced this trace. Mirrors AssistantMessage.modelUsed
  // so the reasoning bubble can carry the same chip.
  modelUsed?: "default" | "secondary";
}

// Per-entry shape carried by the tool_filter bubble. Phase-1 entries also
// carry a cosine similarity score so the UI can rank them visually.
export interface ToolFilterPhase1Persisted {
  toolId: string;
  name: string;
  description: string;
  score: number;
}
export interface ToolFilterEntryPersisted {
  toolId: string;
  name: string;
  description: string;
}

export interface ToolFilterMessage extends MessageBase {
  role: "tool_filter";
  status: "complete" | "error";
  // Candidates ranked by embedding cosine (phase-1 RAG).
  phase1?: ToolFilterPhase1Persisted[];
  // Subset kept by the second-pass LLM filter, if enabled.
  phase2?: ToolFilterEntryPersisted[];
  // Tools whose extension declares `alwaysAvailable: true`; included
  // regardless of filtering when the bypass setting is on.
  alwaysAvailable?: ToolFilterEntryPersisted[];
  // Tools the message named directly (e.g. "use the write_memory tool");
  // force-included ahead of the relevance filter, so they're recorded here
  // instead of in phase1/phase2 even though they bypassed both.
  nameMatched?: ToolFilterEntryPersisted[];
  // Enabled MCP tools offered this turn: the always-available ones (offered
  // every turn), plus any whose always-available is off but that matched the
  // turn by name or description. Recorded here so the bubble can list them
  // without forcing itself on.
  mcp?: ToolFilterEntryPersisted[];
  /** Number of tools actually exposed to the model this turn (post-filter,
   *  post-grant-gating). The phase arrays can't stand in for this: with
   *  filtering disabled they're empty while tools are still sent. The
   *  client uses it to mirror the tools hint into the system bubble only
   *  when core really appended it. */
  toolsSent?: number;
  errorMessage?: string;
}

// Per-entry shape carried by the memory_filter bubble: one indexed memory
// scored against the turn's query by embedding cosine similarity.
export interface MemoryFilterEntryPersisted {
  memoryId: string;
  kind: "knowledge" | "skill";
  title: string;
  summary: string;
  score: number;
}

export interface MemoryFilterMessage extends MessageBase {
  role: "memory_filter";
  status: "complete" | "error";
  // Memories whose summary scored above the relevance floor, ranked by
  // cosine. Empty when nothing matched (the client hides the bubble unless the
  // "show empty selections" toggle is on).
  relevant?: MemoryFilterEntryPersisted[];
  errorMessage?: string;
}

// Content payload a tool pushes to the chat via the one-way display API
// (`ctx.display.*`) or the `show_memory` builtin. Rendered by the client
// as a standalone display bubble; never sent to the LLM.
export type DisplayContent =
  | { type: "markdown"; markdown: string }
  | { type: "image"; dataB64: string; mime: string; alt?: string }
  | { type: "table"; columns: string[]; rows: string[][] }
  | { type: "diff"; before: string; after: string; title?: string };

export interface DisplayMessage extends MessageBase {
  role: "display";
  // Tool call that produced this display, when one did. Lets the UI group
  // the bubble near its originating call.
  callId?: string;
  content: DisplayContent;
}

export interface ErrorMessage extends MessageBase {
  role: "error";
  content: string;
  // Optional structured error context (provider error, scheduler error, etc.).
  code?: string;
  details?: Record<string, unknown>;
}

export type Message =
  | UserMessage
  | AssistantMessage
  | SystemMessage
  | ToolMessage
  | ReasoningMessage
  | ToolFilterMessage
  | MemoryFilterMessage
  | DisplayMessage
  | ErrorMessage;

export interface Session {
  id: string;
  ownerClientId: string;
  title: string;
  createdAtMs: number;
  updatedAtMs: number;
  tokenUsage?: TokenUsage;
  /** A temporary session lives only in core RAM: it never appears in the
   *  session list, is never written to disk, and is discarded the moment the
   *  client navigates away (or the core restarts). Fixed at creation. */
  temporary?: boolean;
}

/** One labelled snippet in a session-list summary. The role is symbolic so
 *  the client can render its own labels (e.g. the configured agent name). */
export interface SummaryPart {
  role: "user" | "agent";
  text: string;
}

export interface SessionListEntry {
  id: string;
  title: string;
  createdAtMs: number;
  updatedAtMs: number;
  messageCount: number;
  /** Length-limited snippets of the opening user/agent exchange, computed
   *  server-side. Empty when the session has no user or assistant messages
   *  yet. */
  summary: SummaryPart[];
}
