// Per-turn LLM stream muxer. Owns one `runOneTurn` worth of streaming state:
// the born-on-first-delta reasoning/assistant skeletons, the delta coalescer
// (one ws frame per ~30ms instead of per token), the reasoning-before-content
// finalize ordering, tool-call assembly with streamId-namespaced callIds, and
// the usage/truncation accounting. Returns the turn's resolved messages + tool
// calls for the orchestrator to finalize and feed back into the hop loop.
//
// Behavior-preserving invariants (do not reorder): reasoning is finalized at the
// first content delta (or on the exit path); reasoning deltas after the first
// content delta are dropped; pending deltas flush before any final snapshot; the
// tail flushes on both success and catch; the callId namespace
// `${streamId}:${idx}:${asm.callId}` is load-bearing for concurrent-turn prompt
// routing and the resume protocol.

import type {
  AssistantMessage,
  ErrorCode,
  ReasoningMessage,
  ServerToClientFrame,
  ToolCall,
} from "@loca/shared";
import { sessionsRepo } from "./sessions-store.ts";
import type { ActiveStream, ResolvedPendingCall } from "./chat-types.ts";
import type { TurnWriter } from "./chat-turn-writer.ts";
import { enabledToolsByName } from "./chat-tool-selection.ts";
import { classifyProviderError } from "./chat-provider-errors.ts";
import { llmScheduler } from "./llm-scheduler.ts";
import { type LlmDelta, type LlmEndpointConfig, type LlmRequest } from "./llm-provider.ts";
import { AppError } from "../platform/errors.ts";
import { getLogger } from "../platform/log.ts";
import { newCallId, newMessageId } from "../platform/ids.ts";

const log = getLogger("chat");

export interface TurnResult {
  assistant?: AssistantMessage;
  reasoning?: ReasoningMessage;
  toolCalls: ResolvedPendingCall[];
  interrupted?: boolean;
  truncated?: boolean;
  error?: { code: ErrorCode; message: string };
}

export class StreamMuxer {
  constructor(
    private readonly stream: ActiveStream,
    private readonly send: (clientId: string, frame: ServerToClientFrame) => void,
    private readonly writer: TurnWriter,
  ) {}

  async run(
    endpoint: LlmEndpointConfig,
    req: LlmRequest,
    route: "default" | "secondary",
  ): Promise<TurnResult> {
    const stream = this.stream;
    const writer = this.writer;
    const isLocal =
      endpoint.baseUrl.includes("127.0.0.1") || endpoint.baseUrl.includes("localhost");
    let assistantContent = "";
    let reasoning = "";
    // Skeletons born on the first delta of each kind, so the client gets a
    // server-minted id (and position) before any chat.delta arrives. The
    // returned messages are these same objects with their content filled in;
    // the caller finalizes them.
    let assistantMsg: AssistantMessage | null = null;
    let reasoningMsg: ReasoningMessage | null = null;
    // Wall-clock anchors for `reasoningDurationMs`: set on the first
    // reasoning chunk and the first content chunk respectively. The pair
    // gives "Thought for Xs" without a recompute from message timestamps.
    let reasoningStartedAtMs: number | null = null;
    let contentStartedAtMs: number | null = null;
    const toolAssemblers = new Map<
      number,
      {
        callId: string;
        extensionId: string;
        toolName: string;
        argsBuffer: string;
      }
    >();
    let usage: { prompt: number; completion: number; total: number } | undefined;
    // Last finish_reason the provider reported. "length" means the model hit
    // the context window rather than stopping naturally, so the reply is cut
    // off (possibly empty, when all the room went to thinking).
    let lastFinishReason: string | null = null;

    // Coalesce outgoing content/reasoning deltas over a short window instead of
    // one ws frame per token. A fast local model emits many small tokens; one
    // JSON.stringify + ws.send each dominates the cost and grows the send buffer
    // under a slow consumer. We still accumulate every token into the saved
    // message synchronously (below), so coalescing is lossless; only the wire
    // frames are batched. The trailing tail is flushed in `finally`.
    const COALESCE_MS = 30;
    let pendingContent = "";
    let pendingReasoning = "";
    let flushTimer: ReturnType<typeof setTimeout> | undefined;
    const flushDeltas = () => {
      flushTimer = undefined;
      if (pendingReasoning && reasoningMsg) {
        this.send(stream.clientId, {
          kind: "chat.delta",
          streamId: stream.streamId,
          messageId: reasoningMsg.id,
          delta: pendingReasoning,
        });
        pendingReasoning = "";
        // Advance the live ref to exactly what has now been EMITTED (born +
        // deltas). Kept here, not on every append, so a mid-stream resubscribe's
        // catch-up snapshot never overlaps the not-yet-flushed pending buffer and
        // double-counts it. Between flushes the ref lags by one COALESCE_MS batch.
        reasoningMsg.content = reasoning;
      }
      if (pendingContent && assistantMsg) {
        this.send(stream.clientId, {
          kind: "chat.delta",
          streamId: stream.streamId,
          messageId: assistantMsg.id,
          delta: pendingContent,
        });
        pendingContent = "";
        assistantMsg.content = assistantContent;
      }
    };
    const scheduleFlush = () => {
      if (flushTimer === undefined) {
        flushTimer = setTimeout(flushDeltas, COALESCE_MS);
      }
    };
    // Reasoning is finalized HERE, not by the caller: at the first content
    // delta when the model produced a reply (so the thought bubble closes
    // with its duration while the reply still streams), otherwise on the
    // exit path. Pending deltas are flushed first so no chat.delta for the
    // id can trail its final snapshot.
    let reasoningFinalized = false;
    const finalizeReasoning = async (interrupted: boolean): Promise<void> => {
      if (!reasoningMsg || reasoningFinalized) return;
      reasoningFinalized = true;
      if (flushTimer !== undefined) clearTimeout(flushTimer);
      flushDeltas();
      reasoningMsg.content = reasoning;
      const endMs = contentStartedAtMs ?? Date.now();
      reasoningMsg.reasoningDurationMs = Math.max(0, endMs - (reasoningStartedAtMs ?? endMs));
      reasoningMsg.pairedAssistantId = assistantMsg?.id;
      if (interrupted) reasoningMsg.interrupted = true;
      await writer.finalize(reasoningMsg);
    };
    // Fills the streamed text into the skeletons. Called on every exit path
    // so a partial (interrupted / errored) message is returned for
    // finalization rather than dropped.
    const settleMessages = async (interrupted: boolean): Promise<void> => {
      await finalizeReasoning(interrupted);
      if (assistantMsg) {
        assistantMsg.content = assistantContent;
        if (interrupted) assistantMsg.interrupted = true;
      }
    };

    const llmStartedAt = Date.now();
    log.info(
      `stream ${stream.streamId}: llm call starting ` +
        `(${isLocal ? "local" : "external"} model ${endpoint.model}, route ${route}, ` +
        `${req.messages.length} messages${req.tools?.length ? `, ${req.tools.length} tools` : ""})`,
    );
    // Completion summary shared by the success and interrupt exits. Token
    // counts come from the stream's usage chunk; absent (interrupt, provider
    // without usage support) we still report the wall-clock time.
    const llmDoneLine = (note: string) => {
      const elapsedMs = Date.now() - llmStartedAt;
      let tokens = "";
      if (usage) {
        const rate = elapsedMs > 0 ? ((usage.completion / elapsedMs) * 1000).toFixed(1) : "?";
        tokens = ` (${usage.prompt} prompt + ${usage.completion} completion tokens, ${rate} token/s)`;
      }
      return `stream ${stream.streamId}: llm call ${note} in ${elapsedMs}ms${tokens}`;
    };

    try {
      for await (const delta of llmScheduler().schedule(req, {
        clientId: stream.clientId,
        isLocal,
      })) {
        if (delta.finishReason) lastFinishReason = delta.finishReason;
        await this.handleDelta(delta, {
          appendContent: async (s) => {
            if (contentStartedAtMs === null) {
              contentStartedAtMs = Date.now();
              assistantMsg = {
                id: newMessageId(),
                ord: -1,
                role: "assistant",
                content: "",
                createdAtMs: Date.now(),
                modelUsed: route,
              };
              writer.born(assistantMsg);
              await finalizeReasoning(false);
            }
            assistantContent += s;
            // NB: the live ref's content is advanced in flushDeltas (to the last
            // FLUSHED position), not here at the bleeding edge, so a mid-stream
            // resubscribe's born snapshot + future deltas don't double-count the
            // pending tail. settleMessages() sets the full content at finalize.
            pendingContent += s;
            scheduleFlush();
          },
          appendReasoning: (s) => {
            // Interleaving rule: reasoning deltas arriving after the first
            // content delta are dropped; the thought bubble is closed once
            // the reply starts.
            if (contentStartedAtMs !== null) return;
            if (reasoningStartedAtMs === null) {
              reasoningStartedAtMs = Date.now();
              reasoningMsg = {
                id: newMessageId(),
                ord: -1,
                role: "reasoning",
                content: "",
                createdAtMs: Date.now(),
                modelUsed: route,
              };
              writer.born(reasoningMsg);
            }
            reasoning += s;
            // Live ref advanced in flushDeltas (last flushed position), not at the
            // bleeding edge; finalizeReasoning() sets the full content at finalize.
            pendingReasoning += s;
            scheduleFlush();
          },
          updateToolCall: (idx, chunk) => {
            let asm = toolAssemblers.get(idx);
            if (!asm) {
              asm = {
                callId: chunk.id ?? newCallId(),
                extensionId: "",
                toolName: chunk.name ?? "",
                argsBuffer: "",
              };
              toolAssemblers.set(idx, asm);
            } else if (chunk.name) {
              asm.toolName = chunk.name;
            }
            if (chunk.id) asm.callId = chunk.id;
            if (chunk.argumentsDelta) asm.argsBuffer += chunk.argumentsDelta;
          },
          captureUsage: (u) => {
            usage = u;
          },
        });
      }
    } catch (err) {
      // User interrupt: not an error. Partial messages are returned flagged
      // `interrupted` so the caller finalizes them and live === reload.
      if (stream.abort.signal.aborted) {
        log.info(llmDoneLine("interrupted"));
        await settleMessages(true);
        return {
          assistant: assistantMsg ?? undefined,
          reasoning: reasoningMsg ?? undefined,
          toolCalls: [],
          interrupted: true,
        };
      }
      await settleMessages(true);
      const classified =
        err instanceof AppError
          ? { code: err.code, message: err.message }
          : classifyProviderError(err);
      log.error(
        `stream ${stream.streamId}: llm call failed (${classified.code}): ${classified.message}`,
      );
      return {
        assistant: assistantMsg ?? undefined,
        reasoning: reasoningMsg ?? undefined,
        toolCalls: [],
        error: classified,
      };
    } finally {
      // Flush any buffered tail on both the success and error paths (runs before
      // the catch's `return` completes), so trailing tokens are never dropped.
      if (flushTimer !== undefined) clearTimeout(flushTimer);
      flushDeltas();
    }
    // A user interrupt usually lands here, not in the catch: the OpenAI SDK
    // swallows the AbortError and simply ends the iteration.
    if (stream.abort.signal.aborted) {
      log.info(llmDoneLine("interrupted"));
      await settleMessages(true);
      return {
        assistant: assistantMsg ?? undefined,
        reasoning: reasoningMsg ?? undefined,
        toolCalls: [],
        interrupted: true,
      };
    }
    // Resolve tool names to (extensionId, name) pairs.
    const allEnabled = enabledToolsByName();
    const resolved: ResolvedPendingCall[] = [];
    for (const [idx, asm] of toolAssemblers.entries()) {
      // Namespace the controller/correlation id by streamId (and the tool-call
      // index) so two concurrent turns whose models reuse an id like "call_1"
      // can't collide in the shared inFlightControllers map: a collision there
      // makes every askUser/permission/cancel forward fail the clientId guard
      // and hang both prompts. The model only needs the assistant's
      // tool_calls[].id and the tool result's tool_call_id to match each OTHER
      // within a request, which they still do (both carry this same value).
      const callId = `${stream.streamId}:${idx}:${asm.callId}`;
      const found = allEnabled.get(asm.toolName);
      if (!found) {
        // Tool not found / disabled / grants missing. Emit error msg for the
        // model on next hop.
        resolved.push({
          callId,
          extensionId: "unknown",
          toolName: asm.toolName,
          arguments: asm.argsBuffer || "{}",
          unknown: true,
        });
        continue;
      }
      resolved.push({
        callId,
        extensionId: found.extensionId,
        toolName: asm.toolName,
        arguments: asm.argsBuffer || "{}",
      });
    }

    // A tool-call-only turn (no content) still needs an assistant message:
    // the persisted toolCalls are what lets a later transcript rebuild
    // replay the model's tool_calls. It was never born (no content deltas),
    // so its first chat.message emission is the finalization.
    if (!assistantMsg && resolved.length > 0) {
      assistantMsg = {
        id: newMessageId(),
        ord: -1,
        role: "assistant",
        content: "",
        createdAtMs: Date.now(),
        modelUsed: route,
      };
    }
    if (assistantMsg && resolved.length > 0) {
      assistantMsg.toolCalls = resolved.map<ToolCall>((r) => ({
        callId: r.callId,
        extensionId: r.extensionId,
        toolName: r.toolName,
        arguments: r.arguments,
        status: "pending",
      }));
    }
    await settleMessages(false);
    // Truncation: the model hit the context window (finish_reason "length")
    // on a non-tool turn. The reply is cut off; when thinking consumed the
    // whole window there's no content at all, so synthesize an empty assistant
    // to carry the "cut off" note. (Tool turns finish_reason "tool_calls".)
    const truncated = lastFinishReason === "length" && resolved.length === 0;
    if (truncated) {
      if (!assistantMsg) {
        assistantMsg = {
          id: newMessageId(),
          ord: -1,
          role: "assistant",
          content: "",
          createdAtMs: Date.now(),
          modelUsed: route,
        };
      }
      assistantMsg.truncated = true;
    }
    log.info(llmDoneLine(truncated ? "truncated (context full)" : "done"));
    if (usage) {
      this.send(stream.clientId, {
        kind: "chat.usage",
        streamId: stream.streamId,
        tokenUsage: usage,
      });
      await sessionsRepo().setTokenUsage(stream.sessionId, usage);
    }

    return {
      assistant: assistantMsg ?? undefined,
      reasoning: reasoningMsg ?? undefined,
      toolCalls: resolved,
      truncated,
    };
  }

  private async handleDelta(
    delta: LlmDelta,
    sink: {
      appendContent: (s: string) => void | Promise<void>;
      appendReasoning: (s: string) => void;
      updateToolCall: (
        idx: number,
        chunk: {
          id?: string;
          name?: string;
          argumentsDelta?: string;
        },
      ) => void;
      captureUsage: (u: { prompt: number; completion: number; total: number }) => void;
    },
  ): Promise<void> {
    if (delta.contentDelta) await sink.appendContent(delta.contentDelta);
    if (delta.reasoningDelta) sink.appendReasoning(delta.reasoningDelta);
    if (delta.toolCalls) {
      for (const tc of delta.toolCalls) {
        sink.updateToolCall(tc.index, {
          id: tc.id,
          name: tc.name,
          argumentsDelta: tc.argumentsDelta,
        });
      }
    }
    if (delta.usage) sink.captureUsage(delta.usage);
  }
}
