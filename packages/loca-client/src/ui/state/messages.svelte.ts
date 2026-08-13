/**
 * The active conversation's messages array and the structural mutations on
 * it (add, edit, delete, regenerate, plus tool / tool_filter / system bubble
 * helpers). Sister slices own the rest:
 *
 *   - sessionsState: session list + active id + title + load/new/delete
 *   - streamingState: live LLM stream flags + buffered chunks + TTS feed +
 *     interruptStreaming orchestration
 *
 * Persistence is server-side now: addUserMessage / updateUserMessage /
 * deleteUserMessage POST/PATCH/DELETE against cores().api().sessions before
 * mutating local state. Tool-call and assistant-content mutations are
 * driven by WS frames the server emits and don't round-trip back.
 */

import type { Message as ServerMessage, ServerToClientFrame } from "@loca/shared";
import {
  type AskUserAnswer,
  type AskUserQuestion,
  asMessageContent,
  type Attachment,
  getTextContent,
  makeMessageId,
  type Message,
  type MessageContent,
  type MessagePart,
  type TokenUsage,
  type ToolCallLogLine,
  type ToolCallPermissionState,
} from "$lib/util/types";
import { cores } from "$lib/core";
import { getLogger } from "$lib/util/log";
import { sessionsState } from "./sessions.svelte";
import { snippetsState } from "./snippets.svelte";
import {
  ensureMarkdownExtension,
  imageExtFromMime,
  utf8ToBase64,
  writeSessionAttachment,
} from "$lib/chat/attachments";
import { applySnippets } from "$lib/snippets/snippets";
import {
  applySystemPromptOverride,
  buildContextBlock,
  buildSystemPrompt,
  buildSystemPromptBase,
} from "$lib/prompts/system-prompt";

type SendMessagesHandler = (anchorUserId?: string) => Promise<void>;

// The live-stream control surface the edit/delete/regenerate flows drive
// (interrupt + abort + a reset, plus an isActive read). It lives in
// streamingState, which imports this slice, so it is injected (setStreamControl)
// rather than imported, keeping the static graph one-way (streaming -> messages,
// never back). Same pattern as setLLMHandlers below.
interface StreamControl {
  readonly isActive: boolean;
  resetTTSPlayback(): void;
  interruptStreaming(): Promise<void>;
  abortSilently(): void;
}

const log = getLogger("messages");

class MessagesState {
  messages = $state<Message[]>([]);
  tokenUsage = $state<TokenUsage | null>(null);

  /** Any tool call bubble currently in a non-terminal state. Drives the
   *  unified "interrupt" affordance in UserInput so tool calls can be stopped
   *  the same way an LLM stream can. */
  hasActiveToolCall = $derived(
    this.messages.some(
      (m) =>
        m.role === "tool" &&
        (m.status === "pending" || m.status === "running" || m.status === "awaiting_user"),
    ),
  );

  // The LLM dispatch needs to be invoked from this slice but lives in a
  // module that imports messagesState itself, so registering the callback
  // here keeps the static import graph one-way (llm -> messages, never back).
  // The handler accepts an optional `anchorUserId` so edit / reprocess can
  // regenerate the turn for an arbitrary user message in the middle of
  // history; called with no argument it sends the most recent user message
  // as a fresh turn at the tail.
  private sendMessagesHandler: SendMessagesHandler | null = null;

  setLLMHandlers(send: SendMessagesHandler): void {
    this.sendMessagesHandler = send;
  }

  // Injected by streamingState at module load (it imports this slice). Calls are
  // null-safe for the brief window before wiring, matching sendMessagesHandler.
  private streamControl: StreamControl | null = null;

  setStreamControl(control: StreamControl): void {
    this.streamControl = control;
  }

  /** Drop the active session's transcript from memory. Called by
   *  sessionsState on every session-boundary transition; sister slices
   *  (streamingState, sessionsState itself) reset their own per-session
   *  fields. */
  clear(): void {
    this.messages = [];
    this.tokenUsage = null;
  }

  /** Replace the in-memory transcript with a session loaded from disk.
   *  Sessions slice owns id/title and is responsible for any defensive
   *  snapshot fixups (e.g. backfilling ids, freezing stuck spinners) before
   *  passing the array in. */
  hydrate(messages: Message[], tokenUsage: TokenUsage | null): void {
    this.messages = messages;
    this.tokenUsage = tokenUsage;
  }

  addMessage(message: Message) {
    if (!message.id) message.id = makeMessageId();
    this.messages.unshift(message);
  }

  /** Splice every non-user bubble between `anchorUserId` and the next-newer
   *  user message. Returns the removed ids. Used by `regenerateTurn` (edit /
   *  reprocess) and `deleteUserMessage` to clear a whole multi-bubble turn
   *  atomically; both edges are preserved (the anchor user and the next user
   *  message stay put).
   *
   *  `serverDelete` controls whether the removals are propagated over REST.
   *  Regenerate paths pass false: the chat.start they trigger carries the
   *  anchor and the server deletes the turn itself (its message_deleted
   *  frames reconcile via removeById, which is idempotent against this
   *  local splice). Plain deletes pass true. */
  private spliceTurnAfter(anchorUserId: string, serverDelete: boolean): string[] {
    const userIdx = this.messages.findIndex((m) => m.role === "user" && m.id === anchorUserId);
    if (userIdx < 0) return [];
    // newest-first: walk from userIdx-1 down toward 0, stop at the first
    // other user message (or the array head). Everything in between is part
    // of this turn.
    let stopIdx = -1;
    for (let i = userIdx - 1; i >= 0; i--) {
      if (this.messages[i].role === "user") {
        stopIdx = i;
        break;
      }
    }
    const startIdx = stopIdx + 1;
    const count = userIdx - startIdx;
    const removedIds: string[] = [];
    if (count > 0) {
      const removed = this.messages.splice(startIdx, count);
      for (const m of removed) if (m.id) removedIds.push(m.id);
    }
    if (serverDelete) void this.serverDeleteMessages(removedIds);
    return removedIds;
  }

  /** Best-effort batch delete on the paired core. Silently swallows per-id
   *  failures so a transient network error doesn't break the UI flow. */
  private async serverDeleteMessages(ids: string[]): Promise<void> {
    const sessionId = sessionsState.id;
    if (!sessionId || ids.length === 0) return;
    const api = cores().api().sessions;
    for (const id of ids) {
      try {
        await api.deleteMessage(sessionId, id);
      } catch (e) {
        log.warn(`server delete ${id} failed:`, e);
      }
    }
  }

  /**
   * Flush any pending attachments to disk under the current session directory,
   * then add the resulting user message. Session ID is assigned eagerly so
   * attachments get a stable path even before the first save.
   */
  async addUserMessage(payload: {
    text: string;
    attachments: Attachment[];
    timestamp: number;
    systemPromptOverride?: string;
    effectiveSystemPrompt?: string | null;
  }): Promise<void> {
    // Sending a new message always preempts any previous assistant TTS that
    // might still be playing or queued.
    this.streamControl?.resetTTSPlayback();

    const trimmedText = payload.text.trim();
    if (!sessionsState.id) {
      // Mint the session server-side so the rest of the flow can reference
      // a real ULID. Falls back to a local timestamp ID if the call fails
      // (e.g. offline core). The next persist attempt will still try.
      try {
        const created = await cores().api().sessions.create(undefined, {
          temporary: sessionsState.isTemporary,
        });
        sessionsState.id = created.id;
        sessionsState.createdAtMs = created.createdAtMs ?? null;
        sessionsState.title = sessionsState.title || sessionsState.defaultTitle;
        // The new session must show up in the list right away: SessionBar
        // renders its buttons only while the list is non-empty.
        void sessionsState.loadList();
      } catch (e) {
        log.warn("session create failed; using local id:", e);
        sessionsState.id = payload.timestamp.toString();
        sessionsState.createdAtMs = payload.timestamp;
        sessionsState.title = sessionsState.title || sessionsState.defaultTitle;
      }
    }
    const sessionId = sessionsState.id;
    const messageId = payload.timestamp.toString();

    const parts: MessagePart[] = [];
    if (trimmedText) parts.push({ type: "text", text: trimmedText });

    for (const att of payload.attachments) {
      try {
        if (att.type === "image") {
          const mime = att.mime || "image/png";
          const ext = imageExtFromMime(mime);
          const filename = att.filename.includes(".")
            ? att.filename
            : `${att.filename || "image"}.${ext}`;
          const written = await writeSessionAttachment(
            sessionId,
            messageId,
            filename,
            att.pendingData,
            mime,
          );
          parts.push({
            type: "image_file",
            filename: written.filename,
            path: written.path,
            mime,
          });
        } else {
          const filename = ensureMarkdownExtension(att.filename || "document.md");
          const written = await writeSessionAttachment(
            sessionId,
            messageId,
            filename,
            utf8ToBase64(att.pendingData),
            "text/markdown",
          );
          parts.push({
            type: "document_file",
            filename: written.filename,
            path: written.path,
          });
        }
      } catch (e) {
        log.error("Failed to upload attachment:", e);
      }
    }

    const content: MessageContent =
      parts.length === 1 && parts[0].type === "text" ? parts[0].text : parts;
    const userMsg: Message = { id: messageId, role: "user", content };
    if (payload.systemPromptOverride) {
      userMsg.systemPromptOverride = payload.systemPromptOverride;
    }
    this.messages.unshift(userMsg);
    this.upsertSystemMessage(payload.effectiveSystemPrompt ?? buildSystemPrompt());
    // Persist server-side so chat.start sees the message in history.
    await this.persistUserMessage(userMsg);
  }

  /**
   * Keep the system-role message in sync with what was actually sent to the
   * LLM on the most recent turn. Always reflected in the array (and therefore
   * persisted with the session) so toggling `prompts.showSystemPrompt` later
   * can reveal it without a round-trip; the toggle is enforced at render time.
   * Public so the LLM dispatch can refresh the bubble with the turn-final
   * prompt (including the tools hint) right before sending.
   */
  upsertSystemMessage(effective: string | null | undefined): void {
    const existingIdx = this.messages.findIndex((m) => m.role === "system");
    const hasContent = typeof effective === "string" && effective.trim().length > 0;

    if (hasContent) {
      const content = (effective as string).trim();
      if (existingIdx >= 0) {
        this.messages[existingIdx] = {
          ...this.messages[existingIdx],
          content,
        };
      } else {
        this.messages.push({
          id: makeMessageId(),
          role: "system",
          content,
        });
      }
    } else if (existingIdx >= 0) {
      this.messages.splice(existingIdx, 1);
    }
  }

  /** Mirror the tools hint core appended to this turn's system prompt into
   *  the system bubble (see the tool_filter final-snapshot handler in
   *  streaming state). Idempotent: re-sent frames or a hint already present
   *  leave the bubble unchanged. */
  appendSystemToolsHint(hint: string): void {
    if (!hint) return;
    const idx = this.messages.findIndex((m) => m.role === "system");
    if (idx < 0) {
      this.upsertSystemMessage(hint);
      return;
    }
    const content = this.messages[idx].content;
    if (typeof content !== "string" || content.includes(hint)) return;
    this.messages[idx] = {
      ...this.messages[idx],
      content: content ? `${content}\n\n${hint}` : hint,
    };
  }

  private toolIdx(callId: string): number {
    return this.messages.findIndex((m) => m.role === "tool" && m.callId === callId);
  }

  /** Record an askUser request on the bubble so the form renders. */
  setToolCallAskUser(callId: string, requestId: string, questions: AskUserQuestion[]): void {
    const idx = this.toolIdx(callId);
    if (idx < 0) return;
    const m = this.messages[idx];
    this.messages[idx] = {
      ...m,
      status: "awaiting_user",
      ephemera: {
        ...m.ephemera,
        askUser: { requestId, questions, answers: null },
      },
    };
  }

  /** Flip the bubble to its waiting-for-permission state; the decision UI
   *  itself lives in UserInput's permission mode, not the bubble. */
  setToolCallPermissionRequest(callId: string, request: ToolCallPermissionState): void {
    const idx = this.toolIdx(callId);
    if (idx < 0) return;
    const m = this.messages[idx];
    this.messages[idx] = {
      ...m,
      status: "awaiting_permission",
      ephemera: { ...m.ephemera, permissionRequest: request },
    };
  }

  /** Flip the bubble to awaiting_user while a schedule confirm form is
   *  pending; the editable form lives in UserInput's schedule-confirm
   *  mode, not the bubble. */
  setToolCallAwaitingSchedule(callId: string): void {
    const idx = this.toolIdx(callId);
    if (idx < 0) return;
    this.messages[idx] = { ...this.messages[idx], status: "awaiting_user" };
  }

  /** Return the bubble to running after the user decided on the schedule
   *  confirm form. */
  clearToolCallAwaitingSchedule(callId: string): void {
    const idx = this.toolIdx(callId);
    if (idx < 0) return;
    const m = this.messages[idx];
    if (m.status !== "awaiting_user") return;
    this.messages[idx] = { ...m, status: "running" };
  }

  /** Return the bubble to running after the user decided (the verdict's
   *  effect arrives via the normal progress/result/error frames). */
  clearToolCallPermissionRequest(callId: string): void {
    const idx = this.toolIdx(callId);
    if (idx < 0) return;
    const m = this.messages[idx];
    if (m.status !== "awaiting_permission") return;
    this.messages[idx] = {
      ...m,
      status: "running",
      ephemera: { ...m.ephemera, permissionRequest: undefined },
    };
  }

  /** Stash the user's answers on the bubble after submit. The WS layer
   *  forwards them back to the worker; this keeps the UI in the right state
   *  while we wait for the next progress / result event. */
  recordToolCallAskUserAnswers(callId: string, answers: AskUserAnswer[]): void {
    const idx = this.toolIdx(callId);
    if (idx < 0) return;
    const m = this.messages[idx];
    if (!m.ephemera?.askUser) return;
    this.messages[idx] = {
      ...m,
      status: "running",
      ephemera: { ...m.ephemera, askUser: { ...m.ephemera.askUser, answers } },
    };
  }

  /** Append a log line visible in the bubble's details disclosure. */
  appendToolCallLog(callId: string, line: ToolCallLogLine): void {
    const idx = this.toolIdx(callId);
    if (idx < 0) return;
    const m = this.messages[idx];
    const prior = m.ephemera?.logs ?? [];
    const logs = prior.length >= 200 ? prior.slice(-199) : prior.slice();
    logs.push(line);
    this.messages[idx] = { ...m, ephemera: { ...m.ephemera, logs } };
  }

  /** Clear every non-user bubble between `anchorUserId` and the next-newer
   *  user message, then re-stream the turn from that user message. Used as
   *  the single regeneration entry point for both edit (user message
   *  modified, then regenerate) and reprocess (no text change, just regen
   *  the turn that the targeted agent bubble belongs to). Bubbles older
   *  than the anchor and bubbles at or newer than the next user message
   *  stay untouched. */
  private async regenerateTurn(anchorUserId: string): Promise<void> {
    // Local-only splice for instant feedback; the chat.start this triggers
    // carries the anchor, the server deletes the old turn itself, and its
    // message_deleted frames reconcile (removeById is idempotent).
    this.spliceTurnAfter(anchorUserId, false);
    await this.sendMessagesHandler?.(anchorUserId);
  }

  /** Update any user message by id and regenerate the turn. If no id is
   *  provided, falls back to the most recent user message. */
  async updateUserMessage(messageId: string | undefined, content: MessageContent) {
    await this.streamControl?.interruptStreaming();
    // interruptStreaming only stops TTS if something was actively streaming.
    // When editing a message with a settled assistant response, the response
    // is about to be discarded (either spliced out on delete, or replaced by
    // the resend), so any replay-mode TTS tied to it must stop too.
    this.streamControl?.resetTTSPlayback();

    const userIdx = messageId
      ? this.messages.findIndex((m) => m.role === "user" && m.id === messageId)
      : this.messages.findIndex((m) => m.role === "user");
    if (userIdx < 0) return;

    const userMsgId = this.messages[userIdx].id;

    const isEmpty =
      typeof content === "string"
        ? !content.trim()
        : content.every((p) => p.type === "text" && !p.text.trim());

    // Reject clearing the sole user message of a session: leaving the session
    // with zero user messages orphans the session file and drops any system
    // message + attachments that belonged to this turn.
    if (isEmpty) {
      const userCount = this.messages.filter((m) => m.role === "user").length;
      if (userCount <= 1) {
        log.warn("refusing to empty the only user message of this session");
        return;
      }
      if (userMsgId) {
        await this.deleteUserMessage(userMsgId);
      }

      return;
    }

    // Re-apply snippet expansion over the edited text so retyped `@triggers`
    // take effect on resend. Attachments are preserved verbatim.
    const rawText = getTextContent(content);
    const { userText, systemOverride } = applySnippets(rawText, snippetsState.snippets);
    const systemPromptOverride = systemOverride
      ? (applySystemPromptOverride(buildSystemPromptBase(), systemOverride, buildContextBlock()) ??
        undefined)
      : undefined;
    const effectiveSystemPrompt = systemPromptOverride ?? buildSystemPrompt();

    const expandedContent: MessageContent = (() => {
      if (typeof content === "string") return userText;
      const nonText = (content as MessagePart[]).filter((p) => p.type !== "text");
      const parts: MessagePart[] = [];
      if (userText) parts.push({ type: "text", text: userText });
      parts.push(...nonText);
      if (parts.length === 1 && parts[0].type === "text") return parts[0].text;
      return parts;
    })();

    const updatedMsg: Message = { role: "user", content: expandedContent };
    if (userMsgId) updatedMsg.id = userMsgId;
    if (systemPromptOverride) {
      updatedMsg.systemPromptOverride = systemPromptOverride;
    }
    this.messages[userIdx] = updatedMsg;

    // Propagate the edit to the server so the next chat.start sees the new
    // content. Awaited: the regenerate below makes the server rebuild the
    // turn from its persisted history, so the patch must land first or the
    // resend conditions on the old text.
    if (userMsgId && sessionsState.id) {
      try {
        const patch: Partial<ServerMessage> = {
          content: expandedContent,
          systemPromptOverride,
        };
        await cores().api().sessions.patchMessage(sessionsState.id, userMsgId, patch);
      } catch (e) {
        log.warn(`server patch ${userMsgId} failed:`, e);
      }
    }

    const isFirstMessage = this.messages.filter((m) => m.role === "user").length === 1;
    if (isFirstMessage) {
      sessionsState.title = sessionsState.defaultTitle;
    }

    this.upsertSystemMessage(effectiveSystemPrompt);

    if (!userMsgId) {
      // Defensive: every user message gets an id at addUserMessage time, but
      // older session files may have been hydrated without one. Without an
      // anchor we can't safely splice the turn, so fall back to a tail send.

      await this.sendMessagesHandler?.();
      return;
    }

    await this.regenerateTurn(userMsgId);
  }

  /** Delete a user message along with every bubble in its turn (everything
   *  between this user message and the next-newer user message). If this
   *  empties the session of user messages, remove the session entirely. */
  async deleteUserMessage(messageId: string) {
    await this.streamControl?.interruptStreaming();
    this.streamControl?.resetTTSPlayback();

    const userIdx = this.messages.findIndex((m) => m.role === "user" && m.id === messageId);
    if (userIdx < 0) return;

    // Splice the turn first (everything newer than the user message, up to
    // the next user message). Then refind the user message's index (it may
    // have shifted) and splice it too. spliceTurnAfter handles the server
    // delete of the turn; we delete the user message separately.
    this.spliceTurnAfter(messageId, true);
    const refreshedUserIdx = this.messages.findIndex(
      (m) => m.role === "user" && m.id === messageId,
    );
    if (refreshedUserIdx >= 0) {
      this.messages.splice(refreshedUserIdx, 1);
    }
    void this.serverDeleteMessages([messageId]);

    const remainingUsers = this.messages.filter((m) => m.role === "user").length;
    if (remainingUsers === 0) {
      await sessionsState.delete();
      return;
    }
  }

  /** Regenerate the entire turn that produced the given assistant/error
   *  bubble. Walks older from the bubble until it hits the user message
   *  that caused the turn, then delegates to `regenerateTurn`. Equivalent
   *  to "edit that user message with no text change." */
  async reprocessAgentMessage(messageId: string) {
    if (this.streamControl?.isActive) return;
    this.streamControl?.resetTTSPlayback();

    const agentIdx = this.messages.findIndex(
      (m) => (m.role === "assistant" || m.role === "error") && m.id === messageId,
    );
    if (agentIdx < 0) return;

    // newest-first: walk older (higher index) until we find the user message
    // that anchors this turn.
    let userMsgId: string | null = null;
    for (let i = agentIdx + 1; i < this.messages.length; i++) {
      if (this.messages[i].role === "user") {
        userMsgId = this.messages[i].id ?? null;
        break;
      }
    }
    if (!userMsgId) return;

    await this.regenerateTurn(userMsgId);
  }

  /** Regenerate the turn anchored on a user message, with no text change.
   *  Symmetric counterpart to `reprocessAgentMessage` for the user message
   *  context menu: equivalent to editing the message and resending it
   *  unchanged. */
  async reprocessUserMessage(messageId: string) {
    if (this.streamControl?.isActive) return;
    this.streamControl?.resetTTSPlayback();

    const userIdx = this.messages.findIndex((m) => m.role === "user" && m.id === messageId);
    if (userIdx < 0) return;

    await this.regenerateTurn(messageId);
  }

  /** Delete an assistant/error message together with the entire turn it
   *  belongs to (every bubble between the user message that caused the turn
   *  and the next-newer user message). The user message itself stays so the
   *  conversation thread is preserved. Safe to call on a currently-streaming
   *  bubble - aborts generation silently first. If we can't locate the
   *  causing user message (orphaned bubble, shouldn't happen), falls back
   *  to in-place delete of the bubble + any paired reasoning. */
  async deleteAgentMessage(messageId: string) {
    const idx = this.messages.findIndex(
      (m) => (m.role === "assistant" || m.role === "error") && m.id === messageId,
    );
    if (idx < 0) return;

    const isStreamingThis = (this.streamControl?.isActive ?? false) && idx === 0;
    if (isStreamingThis) {
      this.streamControl?.abortSilently();
    }
    this.streamControl?.resetTTSPlayback();

    // Find the user message that caused this turn (walk older = higher idx).
    let userMsgId: string | null = null;
    for (let i = idx + 1; i < this.messages.length; i++) {
      if (this.messages[i].role === "user") {
        userMsgId = this.messages[i].id ?? null;
        break;
      }
    }

    if (userMsgId) {
      this.spliceTurnAfter(userMsgId, true);
    } else {
      // Orphaned bubble: nothing to anchor a turn-splice on, so just remove
      // the bubble and any paired reasoning in place.
      const removedIds: string[] = [messageId];
      this.messages.splice(idx, 1);
      const reasoningIdx = this.messages.findIndex(
        (m) => m.role === "reasoning" && m.pairedAssistantId === messageId,
      );
      if (reasoningIdx >= 0) {
        const reasoningId = this.messages[reasoningIdx].id;
        if (reasoningId) removedIds.push(reasoningId);
        this.messages.splice(reasoningIdx, 1);
      }
      void this.serverDeleteMessages(removedIds);
    }

    const remainingUsers = this.messages.filter((m) => m.role === "user").length;
    if (remainingUsers === 0) {
      await sessionsState.delete();
      return;
    }
  }

  // --- server-driven message lifecycle (chat.message / chat.delta) --------

  /** Apply a server message snapshot. The id is server-minted and stable from
   *  birth, so this is a plain upsert: replace in place when the id is known
   *  (preserving the client-only `ephemera` overlay), otherwise insert at the
   *  chronological slot right after `afterId` (newest-first array: just
   *  before the anchor's index). `afterId: null` inserts at the newest
   *  position. */
  applyServerMessage(msg: ServerMessage, afterId: string | null): void {
    // The client bag Message is a superset of every wire role's fields, so a
    // spread is the truthful widening (same conversion onChatMessage uses).
    const local: Message = { ...msg };
    const idx = this.messages.findIndex((m) => m.id === local.id);
    if (idx >= 0) {
      this.messages[idx] = { ...local, ephemera: this.messages[idx].ephemera };
      return;
    }
    if (afterId !== null) {
      const anchorIdx = this.messages.findIndex((m) => m.id === afterId);
      if (anchorIdx >= 0) {
        this.messages.splice(anchorIdx, 0, local);
        return;
      }
    }
    this.messages.unshift(local);
  }

  /** Append a streamed delta to the message's content. Returns the full text
   *  after the append (so the caller can feed TTS) or null when the id is
   *  unknown. */
  appendDelta(messageId: string, delta: string): string | null {
    const idx = this.messages.findIndex((m) => m.id === messageId);
    if (idx < 0) return null;
    const cur = this.messages[idx].content;
    const next = (typeof cur === "string" ? cur : "") + delta;
    this.messages[idx] = { ...this.messages[idx], content: next };
    return next;
  }

  /** Remove a message by id (server-initiated delete). */
  removeById(id: string): void {
    const idx = this.messages.findIndex((m) => m.id === id);
    if (idx >= 0) this.messages.splice(idx, 1);
  }

  /** Apply a server-emitted tool.* frame to the matching bubble. */
  applyToolEvent(
    frame: Extract<
      ServerToClientFrame,
      {
        kind:
          | "tool.progress"
          | "tool.askuser_request"
          | "tool.permission_request"
          | "tool.log"
          | "tool.result"
          | "tool.error"
          | "tool.cancelled";
      }
    >,
  ): void {
    const idx = this.toolIdx(frame.callId);
    if (idx < 0) return;
    const m = this.messages[idx];
    if (frame.kind === "tool.progress") {
      this.messages[idx] = {
        ...m,
        progress: frame.progress,
        label: frame.label,
        description: frame.description,
      };
    } else if (frame.kind === "tool.askuser_request") {
      this.setToolCallAskUser(frame.callId, frame.requestId, frame.questions);
    } else if (frame.kind === "tool.permission_request") {
      this.setToolCallPermissionRequest(frame.callId, {
        requestId: frame.requestId,
        permissionKind: frame.permissionKind,
        resource: frame.resource,
        apiName: frame.apiName,
        declared: frame.declared,
        reason: frame.reason,
      });
    } else if (frame.kind === "tool.log") {
      this.appendToolCallLog(frame.callId, {
        level: frame.level,
        message: frame.message,
        ts: Date.now(),
      });
    } else if (frame.kind === "tool.result") {
      this.messages[idx] = { ...m, status: "completed", result: frame.result };
    } else if (frame.kind === "tool.error") {
      this.messages[idx] = { ...m, status: "failed", error: frame.error };
    } else if (frame.kind === "tool.cancelled") {
      this.messages[idx] = { ...m, status: "cancelled" };
    }
  }

  /** POST a freshly-added user message to the core so chat.start sees it
   *  in the persisted history. Best-effort; failures get logged. */
  private async persistUserMessage(msg: Message): Promise<void> {
    const sessionId = sessionsState.id;
    if (!sessionId) return;
    const content = asMessageContent(msg.content);
    if (msg.role !== "user" || content === null) return;
    // Build the wire user message from the bag's user fields rather than
    // asserting the superset bag IS the union. ord/createdAtMs are
    // server-assigned on append, so they ride as zero placeholders.
    const wire: ServerMessage = {
      role: "user",
      content,
      id: msg.id ?? makeMessageId(),
      ord: msg.ord ?? 0,
      createdAtMs: msg.createdAtMs ?? Date.now(),
      ...(msg.systemPromptOverride ? { systemPromptOverride: msg.systemPromptOverride } : {}),
      ...(msg.automated ? { automated: true } : {}),
    };
    try {
      await cores().api().sessions.appendMessage(sessionId, wire);
    } catch (e) {
      log.warn("persist failed:", e);
    }
  }
}

export const messagesState = new MessagesState();
