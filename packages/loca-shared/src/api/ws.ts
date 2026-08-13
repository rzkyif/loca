// WebSocket frame envelope shapes for /ws/v1.
// Multiplexed: every frame carries the discriminator it needs
// (streamId / callId / jobId) for the client to route it.

import type {
  AskUserAnswer,
  AskUserQuestion,
  AttachmentRef,
  Message,
  Session,
  TokenUsage,
} from "../domain/session.ts";
import type { DownloadEntry, RequiredFile } from "../domain/model.ts";
import type { CoreStatusSnapshot } from "../domain/core-status.ts";
import type { PermissionKind } from "../domain/extension.ts";
import type { ScheduledPromptDraft } from "../domain/scheduled-prompt.ts";
import type { ErrorCode } from "./errors.ts";

// --- Client → Server -------------------------------------------------------

export type ClientToServerFrame =
  | { kind: "ping" }
  // Reply to the server's heartbeat ping (see ServerToClientFrame "ping" and
  // the WS hub's armHeartbeat). Keeps the connection from being dropped.
  | { kind: "pong" }
  | {
      kind: "chat.start";
      streamId: string;
      sessionId: string;
      route?: "default" | "secondary";
      /** Effective system prompt for this turn, composed client-side (base
       *  prompt + context block + snippet overrides). The client owns the
       *  context fields (local date/time, OS, ...), so the final string is
       *  sent per turn; when absent, core falls back to its
       *  `prompts.defaultSystemPrompt` setting. */
      systemPrompt?: string;
      /** Rendered `[toolsAvailable:...]` segment of the context template.
       *  Sent separately from `systemPrompt` because only core knows whether
       *  tools survive the relevance filter: it appends this to the prompt
       *  iff the turn actually exposes tools to the model. */
      toolsHint?: string;
      /** User message anchoring this turn. When set, core deletes every
       *  message strictly between the anchor and the next-newer user
       *  message, truncates the LLM transcript at the anchor (inclusive),
       *  and inserts the new turn's messages into that slot. Absent means
       *  a fresh tail turn anchored on the newest user message. */
      anchorMessageId?: string;
    }
  | { kind: "chat.interrupt"; streamId: string }
  // Ask the core to (re)attach this client to a session: if a turn is still
  // generating on it, the core re-emits the in-flight messages so far (catch-up
  // snapshot) and any outstanding tool prompt, then live deltas resume. Sent on
  // session open and after a reconnect; a no-op when nothing is in flight.
  | { kind: "chat.subscribe"; sessionId: string }
  | {
      kind: "tool.askuser_response";
      callId: string;
      requestId: string;
      answers: AskUserAnswer[];
    }
  | {
      kind: "tool.permission_response";
      callId: string;
      requestId: string;
      allow: boolean;
    }
  | { kind: "tool.cancel"; callId: string }
  // Reply to a schedule.confirm_request. `draft` carries the user's edits
  // when accepted; absent on rejection.
  | {
      kind: "schedule.confirm_response";
      callId: string;
      requestId: string;
      accepted: boolean;
      draft?: ScheduledPromptDraft;
    };

// --- Wire enums ------------------------------------------------------------
//
// Single source of truth for the literal enums that appear in frames below.
// The TS unions here derive from these tuples via `(typeof T)[number]`, and
// the per-frame Zod validators in ../validation/ws.ts reuse the SAME tuples
// via `z.enum(T)`. That is what keeps the type and the runtime validator from
// drifting: a value added here flows to both sides at once. `permissionKind`
// follows the same idea from its own home, `PERMISSION_KINDS` in
// ../domain/extension.ts.

export const CHAT_DONE_REASONS = ["stop", "interrupted", "hop_limit", "length"] as const;
export const TOOL_LOG_LEVELS = ["debug", "info", "warn", "error"] as const;
export const EXTENSION_INSTALL_STREAMS = ["stdout", "stderr"] as const;
export const SESSION_UPDATED_OPS = [
  "title_changed",
  "title_generating",
  "message_added",
  "message_updated",
  "message_deleted",
] as const;
export const SESSION_CREATED_REASONS = ["schedule", "greeting"] as const;
export const SESSION_CREATED_FOCUSES = ["show", "show_when_done"] as const;

// --- Server → Client -------------------------------------------------------

// The askUser question/answer shapes are domain types (also rendered by the
// shared UI), so they live in `../domain/session.ts`; re-exported here because
// the WS frames below carry them.
export type {
  AskUserAnswer,
  AskUserChoiceQuestion,
  AskUserDiffQuestion,
  AskUserFilesQuestion,
  AskUserImageQuestion,
  AskUserQuestion,
  AskUserTableQuestion,
} from "../domain/session.ts";

export type ServerToClientFrame =
  | { kind: "pong" }
  // Server heartbeat ping; the client must reply with a "pong" frame or the WS
  // hub drops the connection after the pong timeout (see armHeartbeat).
  | { kind: "ping" }
  // Chat stream events. The server owns message identity and order: every
  // chat-born message (assistant, reasoning, tool, tool_filter, memory_filter)
  // is announced with a `chat.message` snapshot before any delta touches it, and
  // the same frame kind later carries its persisted terminal form.
  | {
      kind: "chat.message";
      streamId: string;
      sessionId: string;
      /** Full snapshot of one message. The first emission for an id is the
       *  birth; later emissions with the same id replace the message in
       *  place. */
      message: Message;
      /** Chronological insert-after position. Only meaningful on the first
       *  emission for an id; null = insert at the turn anchor (after the
       *  anchoring user message). */
      afterId: string | null;
      /** True when this snapshot is the persisted, terminal form. */
      final: boolean;
    }
  | { kind: "chat.delta"; streamId: string; messageId: string; delta: string }
  | { kind: "chat.usage"; streamId: string; tokenUsage: TokenUsage }
  | {
      kind: "chat.done";
      streamId: string;
      reason: (typeof CHAT_DONE_REASONS)[number];
    }
  | { kind: "chat.error"; streamId: string; code: ErrorCode; message: string }
  // Tool-call events
  | {
      kind: "tool.progress";
      callId: string;
      progress: number;
      label?: string;
      description?: string;
    }
  | {
      kind: "tool.askuser_request";
      callId: string;
      requestId: string;
      questions: AskUserQuestion[];
    }
  // A running tool hit a permission its grants do not cover; the call is
  // paused on Deno's prompt until the user allows or rejects in chat.
  | {
      kind: "tool.permission_request";
      callId: string;
      requestId: string;
      permissionKind: PermissionKind;
      /** What the tool is trying to touch, as reported by Deno (host:port,
       *  path, env key, binary, sys flag; empty for ffi-without-path). */
      resource: string;
      /** The Deno API that triggered the check (e.g. `fetch()`), when known. */
      apiName?: string;
      /** False when the access matches none of the tool's declared
       *  permissions and the extension's undeclared policy is `ask`. */
      declared: boolean;
      /** The declared permission's reason, when declared. */
      reason?: string;
      extensionId: string;
      toolName: string;
    }
  | {
      kind: "tool.log";
      callId: string;
      level: (typeof TOOL_LOG_LEVELS)[number];
      message: string;
    }
  | { kind: "tool.result"; callId: string; result: unknown }
  | { kind: "tool.error"; callId: string; error: string; code?: ErrorCode }
  | { kind: "tool.cancelled"; callId: string }
  // Extension install / lifecycle
  | {
      kind: "extension.install_log";
      jobId: string;
      id: string;
      stream: (typeof EXTENSION_INSTALL_STREAMS)[number];
      line: string;
    }
  | {
      kind: "extension.install_done";
      jobId: string;
      id: string;
      ok: boolean;
      code: number;
    }
  | { kind: "extension.snapshot" }
  // MCP servers: repaint after a config change, connect, or enablement toggle.
  | { kind: "mcp.snapshot" }
  // Downloads + sidecars
  | { kind: "downloads.snapshot"; items: DownloadEntry[] }
  | {
      kind: "requirements.snapshot";
      required: RequiredFile[];
      missing: RequiredFile[];
    }
  // Core settings sync. Broadcast on every core-settings change so all
  // connected clients converge without polling (a client's own PATCH echoes
  // back; value-diffing on the client makes the echo a no-op).
  | {
      kind: "settings.updated";
      // Changed entries of the core's sparse settings store (non-default
      // values). Never contains secret-typed keys.
      values: Record<string, unknown>;
      // Keys deleted from the sparse store (reverted to schema default).
      deleted: string[];
      // Configured secret names; present only when the secret set changed.
      secretNames?: string[];
    }
  // Session sync (title generated server-side, REST message edits, deletes).
  // Streamed chat messages are NOT mirrored here; they arrive as
  // `chat.message` frames instead.
  | {
      kind: "session.updated";
      sessionId: string;
      op: (typeof SESSION_UPDATED_OPS)[number];
      payload?: {
        title?: string;
        generating?: boolean;
        messageId?: string;
        message?: unknown;
        attachments?: AttachmentRef[];
      };
    }
  // Core created a session itself (a scheduled prompt or greeting fired).
  // Broadcast to the owner client so it can refresh its session list and
  // surface the window.
  | {
      kind: "session.created";
      session: Session;
      reason: (typeof SESSION_CREATED_REASONS)[number];
      scheduledPromptId?: string;
      /** "show": navigate to the session and show the window now;
       *  "show_when_done": navigate silently, show the window when the
       *  session's stream finishes. */
      focus: (typeof SESSION_CREATED_FOCUSES)[number];
    }
  // A running tool proposed a scheduled prompt; the call is paused until
  // the user accepts (possibly after editing the draft) or rejects in chat.
  | {
      kind: "schedule.confirm_request";
      callId: string;
      requestId: string;
      draft: ScheduledPromptDraft;
    }
  // Self-update
  | { kind: "update.staged"; version: string }
  | { kind: "update.error"; code: ErrorCode; message: string }
  // Aggregate core lifecycle status (Starting Up / Idle / Busy / Updating /
  // Error), plus the per-subsystem breakdown and (while busy) queue counts that
  // back the CoreBar's expanded detail card. The SINGLE status frame: there is
  // no separate per-sidecar frame; the client rebuilds its per-sidecar facade
  // (`serversState`) from `snapshot.subsystems`. Broadcast to every connected
  // client on each change, the one status frame that is intentionally global
  // (not per-client), since core readiness and load are the same for everyone.
  // The client merges it with its own transport state before display.
  | { kind: "core.status"; snapshot: CoreStatusSnapshot };

export type WsFrame = ClientToServerFrame | ServerToClientFrame;
