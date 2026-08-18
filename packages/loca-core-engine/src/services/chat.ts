// Chat orchestrator. Translates a chat.start WS frame into a streaming
// turn: build provider request, run the LLM stream, dispatch tool calls
// when the model asks for them, and emit chat.* / tool.* frames over the
// WS hub.
//
// The server owns message identity and order. A streamed chat-born message
// (reasoning, assistant, tool) is announced to the client as a `chat.message`
// birth snapshot before any `chat.delta` touches it; the filter bubbles
// (tool_filter, memory_filter) skip the birth and emit only once they reach
// their end state. The same TurnWriter persists each at the turn's insertion
// cursor and emits the terminal snapshot (`final: true`). Live order and persisted order
// therefore converge by construction; the client never mints ids for these
// messages.
//
// Resume: the turn keeps running if its client disconnects (transient drop or a
// core swap). Each ActiveStream buffers its born-but-not-yet-finalized messages
// (in memory) and any open tool prompt; a reconnecting client (same clientId)
// sends `chat.subscribe`, and `resubscribe()` re-emits those born snapshots
// (catch-up) plus the open prompt, to the owning client only, after which live
// deltas resume on the new socket. A tool awaiting user input therefore survives
// a client disconnect / restart transparently to the tool code (the worker stays
// paused); a core restart drops the buffer, which is acceptable.
//
// Every settings key read below is defined in the shared schema
// (`@loca/shared/src/domain/settings/groups/*.ts`). Defaults applied if
// absent (sparse settings.json convention).
//   llm.provider                       : "local" | "external" (endpointResolver)
//   llm.host, llm.port                 : local llama-server (endpointResolver)
//   llm.external.baseUrl/apiKey/model  : external provider (endpointResolver)
//   llm.contextSize, llm.external.contextSize : usage tracking
//   llm.reasoning                      : "off" | "on" (endpointResolver)
//   llm.temperature/topP/topK/minP/repeatPenalty : sampling (endpointResolver)
//   dualModel.enabled                  : boolean (run complexity classifier)
//   dualModel.external.*               : secondary endpoint (endpointResolver)
//   prompts.defaultSystemPrompt        : string (default "")
//   prompts.complexityDetectionPrompt  : string (default constant)
//   tools.enabled                      : boolean (default false)
//   tools.maxHops                      : number (default 5)
//   tools.filteringEnabled             : boolean (default true)
//   tools.filteringMinTools            : number  (default 0, always filter)
//   tools.maxTools                     : number  (default 30, final cap)
//   tools.secondPassEnabled            : boolean (default true)
//   tools.alwaysAvailableEnabled       : boolean (default true)
//   memories.enabled                   : boolean (default true, summary injection)
//   memories.maxRelevant               : number  (default 3)
//   memories.skills.autoRelevancy      : boolean (default true, auto-surface skills)

import type {
  AskUserAnswer,
  ChatStartFrame,
  MemoryFilterMessage,
  ScheduledPromptDraft,
  ServerToClientFrame,
  ToolMessage,
} from "@loca/shared";
import { errMessage } from "@loca/shared";
import { sessionsRepo } from "./sessions-store.ts";
import { boolSetting, numSetting, strSetting } from "./settings-access.ts";
import { toOpenAiMessages } from "./chat-attachments.ts";
import { classifyComplexity } from "./chat-complexity-router.ts";
import type { ActiveStream } from "./chat-types.ts";
import { TurnWriter } from "./chat-turn-writer.ts";
import { buildToolList } from "./chat-tool-selection.ts";
import { StreamMuxer } from "./chat-stream-muxer.ts";
import { type InFlightEntry, ToolDispatcher } from "./chat-tool-dispatch.ts";
import { lastUserId, lastUserText } from "./chat-history.ts";
import { hasMemories, relevantMemories, relevantMemoriesBlock } from "./memory-injection.ts";
import { type LlmRequest } from "./llm-provider.ts";
import { maybeGenerateTitle } from "./title-gen.ts";
import { resolveEndpoint } from "./endpoint-resolver.ts";
import { loadEffective, loadEffectiveResolved } from "./core-settings.ts";
import { frameBus } from "../frame-bus.ts";
import { host } from "../platform/runtime.ts";
import { getLogger } from "../platform/log.ts";
import { newMessageId } from "../platform/ids.ts";

const log = getLogger("chat");

const DEFAULT_MAX_TOOL_HOPS = 5;

export class ChatService {
  private active = new Map<string, ActiveStream>();

  // In-flight tool-call controllers, keyed by callId. The forward* methods (fed
  // by the ws handlers) look a controller up to deliver an askuser/permission/
  // schedule response or a cancel; the dispatcher owns each entry's lifetime.
  private inFlightControllers = new Map<string, InFlightEntry>();

  // Runs each tool call to completion and folds the outcome into its message.
  // Shares the in-flight controller map so the forward*/clearPrompt methods
  // below can reach a call's controller; emitPrompt retains open prompts for
  // resubscribe. Stateless beyond those injected references.
  private dispatcher = new ToolDispatcher(
    (c, f) => this.send(c, f),
    (s, callId, f) => this.emitPrompt(s, callId, f),
    this.inFlightControllers,
  );

  // Called by the WS handler when it sees a chat.start frame. Returns
  // synchronously; the streaming runs in the background and pushes frames
  // via wsHub.
  async start(clientId: string, frame: ChatStartFrame): Promise<void> {
    const session = await sessionsRepo().getOrThrow(clientId, frame.sessionId);
    if (this.hasActiveOn(clientId, session.id)) {
      this.send(clientId, {
        kind: "chat.error",
        streamId: frame.streamId,
        code: "session_busy",
        message: "another stream is active on this session",
      });
      return;
    }
    const abort = new AbortController();
    const stream: ActiveStream = {
      streamId: frame.streamId,
      sessionId: session.id,
      clientId,
      abort,
      liveMessages: new Map(),
      outstandingPrompts: new Map(),
    };
    this.active.set(frame.streamId, stream);
    host().status?.noteActiveStreams(this.active.size);
    // Cancel any pending idle-unload as soon as a turn begins (the model is
    // about to be used). run() reloads it before scheduling if it was unloaded.
    host().status?.noteLlmActivity();
    void this.run(stream, frame)
      .catch((err) => {
        log.error(`stream ${frame.streamId} crashed: ${errMessage(err)}`);
        this.send(clientId, {
          kind: "chat.error",
          streamId: frame.streamId,
          code: "internal_error",
          message: errMessage(err),
        });
      })
      .finally(() => {
        this.active.delete(frame.streamId);
        host().status?.noteActiveStreams(this.active.size);
        // When the last turn ends, arm idle-unload (no-op unless enabled).
        host().status?.onTurnEnd(this.active.size);
      });
  }

  // Control verbs are scoped to the owning client. Stream/call ids are
  // normally delivered only to their owner (via broadcastToClient), but enforce
  // ownership here so one paired client can never interrupt, cancel, or answer
  // an askUser prompt belonging to another client's in-flight chat/tool call.
  interrupt(streamId: string, clientId: string): void {
    const s = this.active.get(streamId);
    if (!s || s.clientId !== clientId) return;
    s.abort.abort();
  }

  forwardAskUserResponse(
    callId: string,
    requestId: string,
    answers: AskUserAnswer[],
    clientId: string,
  ): void {
    const entry = this.inFlightControllers.get(callId);
    if (!entry || entry.clientId !== clientId) return;
    this.clearPrompt(callId);
    entry.ctl.respondAskUser(requestId, answers);
  }

  forwardPermissionResponse(
    callId: string,
    requestId: string,
    allow: boolean,
    clientId: string,
  ): void {
    const entry = this.inFlightControllers.get(callId);
    if (!entry || entry.clientId !== clientId) return;
    this.clearPrompt(callId);
    entry.ctl.respondPermission(requestId, allow);
  }

  /** Settle a schedule confirm: on accept, persist the (possibly edited)
   *  draft for the answering client before unblocking the tool, so the
   *  tool's "scheduled" report is only ever sent for a stored schedule. */
  forwardScheduleResponse(
    callId: string,
    requestId: string,
    accepted: boolean,
    draft: ScheduledPromptDraft | undefined,
    clientId: string,
  ): void {
    const entry = this.inFlightControllers.get(callId);
    if (!entry || entry.clientId !== clientId) return;
    // A response whose requestId is not the open confirm (stale, replayed,
    // or forged) must not persist anything: the insert below would otherwise
    // run once per replay.
    if (!entry.ctl.hasPendingSchedule(requestId)) return;
    this.clearPrompt(callId);
    const confirmed = accepted && draft !== undefined;
    if (confirmed) {
      try {
        host().status?.createScheduledPrompt(clientId, draft);
      } catch (err) {
        log.error(`schedule confirm: persisting the draft failed: ${errMessage(err)}`);
        entry.ctl.respondSchedule(requestId, false);
        return;
      }
    }
    entry.ctl.respondSchedule(requestId, confirmed, confirmed ? draft : undefined);
  }

  forwardCancel(callId: string, clientId: string): void {
    const entry = this.inFlightControllers.get(callId);
    if (!entry || entry.clientId !== clientId) return;
    entry.ctl.cancel();
  }

  /** Session ids with an in-flight turn (the model is generating, or awaiting /
   *  executing a tool). A stream stays in `active` for the whole turn and is
   *  removed in the start() `.finally`, so a session NOT in this set is idle
   *  (only waiting for user input). Used by the storage view to refuse deleting
   *  an active session. */
  activeSessionIds(): Set<string> {
    const out = new Set<string>();
    for (const s of this.active.values()) out.add(s.sessionId);
    return out;
  }

  /** (Re)attach a client to a session after it opens it or reconnects. If a
   *  turn is still generating on this client's session, re-emit the in-flight
   *  messages so far as born snapshots (full live catch-up) plus any open tool
   *  prompt, then live deltas resume naturally (the owning clientId is stable
   *  across reconnect, so broadcastToClient reaches the new socket). A no-op
   *  when nothing is in flight. Targeted at the owning client only: a paired
   *  client that does not own the stream gets nothing. */
  resubscribe(clientId: string, sessionId: string): void {
    const stream = this.findActiveOwned(clientId, sessionId);
    if (!stream) return;
    for (const { message, afterId } of stream.liveMessages.values()) {
      this.send(clientId, {
        kind: "chat.message",
        streamId: stream.streamId,
        sessionId: stream.sessionId,
        message,
        afterId,
        final: false,
      });
    }
    // Re-send any outstanding tool prompt so a reconnected / restarted client
    // can answer it (the worker stayed paused the whole time; see Phase 5).
    for (const frame of stream.outstandingPrompts.values()) {
      this.send(clientId, frame);
    }
  }

  // --- internals --------------------------------------------------------

  private hasActiveOn(clientId: string, sessionId: string): boolean {
    return this.findActiveOwned(clientId, sessionId) !== null;
  }

  private findActiveOwned(clientId: string, sessionId: string): ActiveStream | null {
    for (const s of this.active.values()) {
      if (s.clientId === clientId && s.sessionId === sessionId) return s;
    }
    return null;
  }

  private send(clientId: string, frame: ServerToClientFrame): void {
    frameBus().broadcastToClient(clientId, frame);
  }

  /** Emit a tool prompt (askuser / permission / schedule-confirm) AND retain it
   *  on the stream so a (re)subscribing client gets the open prompt re-sent.
   *  At most one prompt per call is open at a time, so keying by callId is
   *  enough; cleared on response (clearPrompt) or when the call ends. */
  private emitPrompt(stream: ActiveStream, callId: string, frame: ServerToClientFrame): void {
    stream.outstandingPrompts.set(callId, frame);
    this.send(stream.clientId, frame);
  }

  /** Drop a retained prompt once it is answered or its call ends, so a later
   *  resubscribe doesn't re-show an already-handled prompt. */
  private clearPrompt(callId: string): void {
    const entry = this.inFlightControllers.get(callId);
    entry?.stream.outstandingPrompts.delete(callId);
  }

  private async run(stream: ActiveStream, frame: ChatStartFrame): Promise<void> {
    // Effective settings for this turn's owner: shared core config overlaid with
    // the client's own per-client inference knobs (sampling, prompts, tool and
    // memory selection). Automated/scheduled sessions reach here via
    // chatService().start(ownerClientId, ...), so they honor the owner's knobs
    // too. Model/server/provider keys are core-global and unaffected.
    const settings = await loadEffectiveResolved(stream.clientId);
    // The SPARSE record, for the endpoint resolver only. Its `llm.port` fallback
    // is `llmPort()` = 7701 + channelPortOffset(), and the schema's default for
    // that key is the bare "7701", because static schema data cannot express a
    // channel-derived value. Handing it a resolved record therefore points every
    // dev and latest install at the STABLE channel's llama-server, silently
    // undoing the port offsets that let two channels run at once. See D-078.
    // Reload the local model if idle-unload stopped it (no-op otherwise).
    await host().status?.ensureLocalModelLoaded(settings);

    // Resolve the route: client may pin a route explicitly, otherwise run
    // the complexity classifier when dual-model is enabled.
    let route: "default" | "secondary" = frame.route ?? "default";
    if (!frame.route && boolSetting(settings, "dualModel.enabled", false)) {
      const last = lastUserText(await sessionsRepo().listMessages(stream.sessionId));
      if (last) {
        try {
          route = await classifyComplexity(settings, last, stream.abort.signal);
        } catch (err) {
          log.warn(`complexity classifier failed; defaulting to "default": ${errMessage(err)}`);
        }
      }
    }

    const endpoint = await resolveEndpoint(await loadEffective(stream.clientId), route);
    // An external provider can't stream until it's configured: an empty base
    // URL, API key, or model otherwise reaches the OpenAI SDK and fails with an
    // opaque network/auth error on send. Fail fast with an actionable message.
    // Local always resolves a loopback URL plus a placeholder key/model, so this
    // only ever trips for an unconfigured external (or dual-model) provider.
    if (!endpoint.baseUrl || !endpoint.apiKey || !endpoint.model) {
      this.send(stream.clientId, {
        kind: "chat.error",
        streamId: stream.streamId,
        code: "provider_error",
        message:
          "This model provider isn't set up yet. Open Settings and add the " +
          "provider's Base URL, API Key, and Model.",
      });
      return;
    }
    const maxHops = numSetting(settings, "tools.maxHops", DEFAULT_MAX_TOOL_HOPS);
    // The client composes the effective prompt per turn (its context block
    // holds client-local facts like date/time and OS) and sends it on the
    // frame; empty string deliberately means "no system prompt". The core
    // setting is the fallback for frames that don't carry one.
    let systemPrompt =
      frame.systemPrompt !== undefined
        ? frame.systemPrompt
        : strSetting(settings, "prompts.defaultSystemPrompt", "");
    log.debug(
      `system prompt: ${systemPrompt.length} chars ` +
        `(${frame.systemPrompt !== undefined ? "from client" : "from core settings"})`,
    );

    // Resolve the turn anchor: the user message this turn hangs off. An
    // explicit anchorMessageId is a regenerate (edit-and-resend): the old
    // turn's messages are deleted server-side and the new ones are inserted
    // into its slot. Otherwise the turn anchors on the newest user message
    // and inserts at the tail.
    let history = await sessionsRepo().listMessages(stream.sessionId);
    if (frame.anchorMessageId) {
      const removed = await sessionsRepo().deleteTurn(stream.sessionId, frame.anchorMessageId);
      for (const id of removed) {
        this.send(stream.clientId, {
          kind: "session.updated",
          sessionId: stream.sessionId,
          op: "message_deleted",
          payload: { messageId: id },
        });
      }
      if (removed.length > 0) {
        history = await sessionsRepo().listMessages(stream.sessionId);
      }
    }
    const anchorId = frame.anchorMessageId ?? lastUserId(history);
    // Condition the model on the anchor's turn, not anything newer: for a
    // mid-history regenerate the transcript stops at the anchor (inclusive).
    // For a fresh tail turn the anchor is the newest message, so this is a
    // no-op.
    if (anchorId) {
      const anchorIdx = history.findIndex((m) => m.id === anchorId);
      if (anchorIdx !== -1) history = history.slice(0, anchorIdx + 1);
    }
    const writer = new TurnWriter(stream, (c, f) => this.send(c, f), anchorId);

    // Tool path: select the provider tool list (relevance filter + always-
    // available + MCP offering); queryVector is reused by the memory relevance
    // injection below. The tool_filter bubble is finalized here so it lands at
    // the turn's cursor (message ordering stays with the orchestrator).
    const toolSelection = await buildToolList({
      settings,
      route,
      streamId: stream.streamId,
      signal: stream.abort.signal,
      queryText: lastUserText(history),
    });
    const toolList = toolSelection.tools;
    const queryVector = toolSelection.queryVector;
    if (toolSelection.filterMessage) await writer.finalize(toolSelection.filterMessage);

    // The tools hint is rendered client-side (the [toolsAvailable:...]
    // segment of the context template) but belongs in the prompt only on
    // turns where the model actually gets tools, which only core knows.
    if (toolList.length > 0 && frame.toolsHint) {
      systemPrompt = systemPrompt ? `${systemPrompt}\n\n${frame.toolsHint}` : frame.toolsHint;
    }

    // Relevant-memory summaries: same embedding machinery as the tool
    // filter (services/relevance.ts), appended to the system prompt when any
    // indexed memory scores above the floor for this turn's query. The
    // selection is also surfaced as a memory_filter bubble (end state only,
    // like the tool filter); the client hides it when empty unless the user
    // turns on "show empty selections".
    // A bubble is only emitted when the user actually has memories; with none
    // there is nothing to select and an empty bubble on every turn is noise.
    if (boolSetting(settings, "memories.enabled", true) && hasMemories()) {
      const memoryFilterMsg: MemoryFilterMessage = {
        id: newMessageId(),
        ord: -1,
        role: "memory_filter",
        status: "complete",
        createdAtMs: Date.now(),
      };
      try {
        const memories = await relevantMemories(
          lastUserText(history),
          queryVector,
          numSetting(settings, "memories.maxRelevant", 3),
          boolSetting(settings, "memories.skills.autoRelevancy", true),
        );
        const memoriesBlock = relevantMemoriesBlock(memories);
        if (memoriesBlock) {
          systemPrompt = systemPrompt ? `${systemPrompt}\n\n${memoriesBlock}` : memoriesBlock;
        }
        memoryFilterMsg.relevant = memories.map((m) => ({
          memoryId: m.memoryId,
          kind: m.kind,
          title: m.title,
          summary: m.summary,
          score: m.score,
        }));
        await writer.finalize(memoryFilterMsg);
      } catch (err) {
        // Non-fatal: the turn just runs without memory context.
        log.warn(`stream ${stream.streamId}: memory relevance failed: ${errMessage(err)}`);
        memoryFilterMsg.status = "error";
        memoryFilterMsg.errorMessage = errMessage(err);
        await writer.finalize(memoryFilterMsg);
      }
    }

    // Resolve any `@resource` / `/prompt` MCP references in the user's message
    // (live server round-trips), appending the fetched blocks to the system
    // prompt. Non-fatal: a failed reference just doesn't expand. The claimed
    // token stems are passed to the memory expander so a slug naming both an MCP
    // resource and a memory expands once (MCP wins), not twice.
    let mcpClaimedTokens = new Set<string>();
    try {
      const mcp = (await host().tools?.resolveMcpTokens(lastUserText(history) ?? "")) ?? {
        block: null,
        claimed: new Set<string>(),
      };
      mcpClaimedTokens = mcp.claimed;
      if (mcp.block) {
        systemPrompt = systemPrompt ? `${systemPrompt}\n\n${mcp.block}` : mcp.block;
      }
    } catch (err) {
      log.warn(`stream ${stream.streamId}: MCP token resolution failed: ${errMessage(err)}`);
    }

    // Reading + base64-encoding image attachments is the costly part of building
    // the provider messages, and the whole transcript is rebuilt every hop.
    // Memoize each attachment's encoded form for the life of this turn so a
    // multi-hop tool conversation reads + encodes each attachment at most once.
    const attachmentCache = new Map<string, string | null>();
    // Hop loop: stream, dispatch tool calls, append tool messages, repeat.
    for (let hop = 0; hop < maxHops; hop++) {
      if (stream.abort.signal.aborted) {
        this.send(stream.clientId, {
          kind: "chat.done",
          streamId: stream.streamId,
          reason: "interrupted",
        });
        return;
      }
      const openaiMessages = await toOpenAiMessages(
        history,
        systemPrompt,
        stream.sessionId,
        attachmentCache,
        mcpClaimedTokens,
      );
      const req: LlmRequest = {
        endpoint,
        messages: openaiMessages,
        tools: toolList.length > 0 ? toolList : undefined,
        signal: stream.abort.signal,
      };

      const muxer = new StreamMuxer(stream, (c, f) => this.send(c, f), writer);
      const { assistant, reasoning, toolCalls, interrupted, truncated, error } = await muxer.run(
        endpoint,
        req,
        route,
      );
      // Reasoning was already finalized inside runOneTurn (at the first
      // content delta, or on its exit path), keeping the TurnWriter
      // invariant that reasoning persists before the assistant; here it only
      // joins the transcript. Partial messages from an interrupt or provider
      // error are finalized too (flagged `interrupted`), so streamed text is
      // never silently lost.
      if (reasoning) history.push(reasoning);
      if (
        assistant &&
        (assistant.content.length > 0 ||
          (assistant.toolCalls?.length ?? 0) > 0 ||
          assistant.truncated)
      ) {
        await writer.finalize(assistant);
        history.push(assistant);
      }
      if (interrupted) {
        this.send(stream.clientId, {
          kind: "chat.done",
          streamId: stream.streamId,
          reason: "interrupted",
        });
        return;
      }
      if (error) {
        this.send(stream.clientId, {
          kind: "chat.error",
          streamId: stream.streamId,
          code: error.code,
          message: error.message,
        });
        return;
      }
      if (toolCalls.length === 0) {
        // Natural stop, or a context-window cutoff (the assistant bubble
        // carries the "cut off" note in that case).
        this.send(stream.clientId, {
          kind: "chat.done",
          streamId: stream.streamId,
          reason: truncated ? "length" : "stop",
        });
        // Async: generate a title from the first turn. The service
        // self-guards if a title already exists, so calling on every
        // stop is just two DB reads in the no-op case.
        void maybeGenerateTitle(stream.sessionId, stream.clientId);
        return;
      }
      // Execute tool calls. Each call's bubble is born `running` before
      // dispatch and finalized with the outcome after, so the client sees
      // the call the moment it starts.
      for (const pending of toolCalls) {
        const toolMsg: ToolMessage = {
          id: newMessageId(),
          ord: -1,
          role: "tool",
          callId: pending.callId,
          extensionId: pending.extensionId,
          toolName: pending.toolName,
          arguments: pending.arguments,
          status: "running",
          createdAtMs: Date.now(),
        };
        writer.born(toolMsg);
        log.info(
          `stream ${stream.streamId}: tool call ${pending.extensionId}/${pending.toolName} ` +
            `(${pending.callId}) starting`,
        );
        const startedAt = Date.now();
        const displays = await this.dispatcher.execute(stream, pending, toolMsg, writer);
        log.info(
          `stream ${stream.streamId}: tool call ${pending.extensionId}/${pending.toolName} ` +
            `(${pending.callId}) ${toolMsg.status} in ${Date.now() - startedAt}ms` +
            (toolMsg.error ? `: ${toolMsg.error}` : ""),
        );
        await writer.finalize(toolMsg);
        // Persist any display bubbles the call pushed, in birth order, AFTER the
        // tool message so their durable order matches the live order the client
        // saw (tool message first, then its displays).
        for (const display of displays) await writer.finalize(display);
        history.push(toolMsg);
      }
    }
    // The conversation up to here is valid and fully persisted; reaching the
    // hop cap is a stop condition, not an error.
    this.send(stream.clientId, {
      kind: "chat.done",
      streamId: stream.streamId,
      reason: "hop_limit",
    });
  }
}

let _instance: ChatService | null = null;
export function chatService(): ChatService {
  if (!_instance) _instance = new ChatService();
  return _instance;
}

// Test-only: drops the cached instance so the next `chatService()` call rebuilds
// against fresh deps (with a fresh in-flight controllers map).
export function __resetForTesting(): void {
  _instance = null;
}
