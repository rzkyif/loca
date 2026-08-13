/**
 * Live LLM-stream layer. The actual LLM call + tool-call orchestration runs
 * server-side in loca-core, and the server owns message identity and order:
 * every chat-born message arrives as a `chat.message` snapshot (birth, then
 * a final persisted form) and its text streams in via `chat.delta` frames
 * keyed by the server-minted message id. This module:
 *   - sends chat.start / chat.interrupt WS frames
 *   - reduces chat.message / chat.delta / tool.* frames into messagesState
 *   - drives the TTS feed cursor from the streaming assistant text
 *   - exposes the `isActive`/`hasActiveWork`/`isLive` surface components read.
 */

import type { ErrorCode, Message as ServerMessage, ServerToClientFrame } from "@loca/shared";
import { type LLMErrorType, makeMessageId, type Message } from "$lib/util/types";
import { stripEmojisForTTS, stripMarkdownForTTS } from "$lib/tts/text";
import { buildToolsHint } from "$lib/prompts/system-prompt";
import { cores } from "$lib/core";
import { getLogger } from "$lib/util/log";
import { Subscriptions } from "$lib/util/subscriptions";
import { messagesState } from "./messages.svelte";
import { permissionState } from "./permissions.svelte";
import { scheduleConfirmState } from "./schedule-confirm.svelte";
import { settingsState } from "./settings.svelte";
import { ttsState } from "./tts.svelte";
import { viewState } from "./view.svelte";

const log = getLogger("streaming");

// How long to wait for the model's first token before treating the turn as
// stalled. The window covers only the "awaiting first token" gaps (initial
// send and each post-tool hop), never tool execution, so a long-running tool
// can't trip it; it guards the case where a provider accepts the request but
// never streams, which would otherwise spin forever. Generous so a slow local
// model doing tool-filtering plus a cold first token isn't cut off.
const FIRST_TOKEN_TIMEOUT_MS = 120_000;

type InterruptListener = () => void | Promise<void>;

// The session surface this stream layer reads: the active session id (chat
// frames target it) and the stream-done notification (pops a "show when done"
// window). sessionsState imports this module, so it injects the port
// (setSessionPort) rather than this module importing it, keeping the static
// graph one-way (sessions -> streaming, never back).
interface SessionPort {
  readonly id: string | null;
  notifyStreamDone(sessionId: string | null): void;
}

class StreamingState {
  isActive = $state(false);
  /** True between sending chat.start (or a tool final mid-turn) and the next
   *  reasoning/assistant birth. Drives the loading-sentinel spinner, covering
   *  both the pre-first-token gap and the between-hops prompt processing on
   *  slow local models. */
  awaitingFirstDelta = $state(false);
  turnAnchorId = $state<string | null>(null);
  streamId = $state<string | null>(null);
  /** Ids of messages born in the in-flight turn that haven't received their
   *  final snapshot yet. Drives per-bubble streaming affordances. */
  liveIds = $state<ReadonlySet<string>>(new Set());

  hasActiveWork = $derived(this.isActive || messagesState.hasActiveToolCall);

  // TTS feed target + cursor into its stripped text. Ids are server-minted
  // and never change, so no handoff rewrites are needed; a new assistant
  // bubble claims the feed only when playback is idle.
  private ttsTargetId: string | null = null;
  private ttsCursor = 0;
  // Whether this turn has seen an assistant birth yet: the first one starts
  // the TTS stream outright, later hops only claim it when idle.
  private turnHadAssistant = false;
  private interruptListeners: InterruptListener[] = [];
  // Fires if the model never produces a first token (see FIRST_TOKEN_TIMEOUT_MS).
  private firstTokenTimer: ReturnType<typeof setTimeout> | null = null;
  private subs = new Subscriptions();
  // Streams this client started and then abandoned (interrupt / disconnect).
  // The server keeps emitting frames until our interrupt lands, so without
  // this the idle-adoption path below would re-latch onto our own dying
  // stream and resurrect its bubble. Bounded so it can't grow unbounded.
  private abandonedStreamIds: string[] = [];
  // Injected by sessionsState at module load (it imports this module). Reads are
  // null-safe for the brief pre-wiring window.
  private session: SessionPort | null = null;

  setSessionPort(port: SessionPort): void {
    this.session = port;
  }

  isLive(id: string | undefined): boolean {
    return id !== undefined && this.liveIds.has(id);
  }

  onInterrupt(fn: InterruptListener): () => void {
    this.interruptListeners.push(fn);
    return () => {
      const idx = this.interruptListeners.indexOf(fn);
      if (idx >= 0) this.interruptListeners.splice(idx, 1);
    };
  }

  attach(): void {
    this.subs.attach(() => [
      cores().subscribeWs((f) => this.onFrame(f)),
      // A transient transport drop silently stops the frame stream, so isActive
      // (and the spinner) would hang forever. But the turn keeps running
      // server-side: detach transparently rather than interrupt it (see
      // detachForResume). A deliberate core swap closes the socket WITHOUT
      // emitting "disconnected", so it does NOT reach here; the core-switch path
      // (+page.svelte) calls detachForResume directly instead.
      cores().subscribeConnectionState((state) => {
        if (state !== "disconnected") return;
        this.detachForResume();
      }),
    ]);
  }

  detach(): void {
    this.subs.detach();
  }

  resetTTSPlayback(): void {
    ttsState.reset();
  }

  beginTurn(anchorUserId: string | null): void {
    this.turnAnchorId = anchorUserId;
  }

  start(modelUsed: "default" | "secondary" = "default"): void {
    const sessionId = this.session?.id ?? null;
    if (!sessionId) {
      log.warn("start called without an active session");
      return;
    }
    this.isActive = true;
    this.awaitingFirstDelta = true;
    this.turnHadAssistant = false;
    this.liveIds = new Set();
    this.ttsTargetId = null;
    this.ttsCursor = 0;
    const streamId = makeMessageId();
    this.streamId = streamId;
    // The system bubble holds the effective prompt for this turn (base +
    // context block + snippet overrides), kept current by addUserMessage and
    // the edit flows. Sending its exact content keeps the bubble truthful:
    // what the UI shows as "the system prompt" is what the model receives.
    // The tools hint rides along separately; core appends it iff tools
    // survive the filter, and mirrors that on the tool_filter message's
    // `toolsSent` so onFrame can append the same hint to the bubble.
    const systemMsg = messagesState.messages.find((m) => m.role === "system");
    const systemPrompt = typeof systemMsg?.content === "string" ? systemMsg.content : undefined;
    const toolsHint = buildToolsHint() || undefined;
    cores()
      .api()
      .chat.start(streamId, sessionId, modelUsed, {
        systemPrompt,
        toolsHint,
        anchorMessageId: this.turnAnchorId ?? undefined,
      });
    this.armFirstTokenWatchdog();
  }

  /** (Re)start the first-token watchdog for the current "awaiting first token"
   *  window. Cleared the moment a reasoning/assistant bubble is born or the turn
   *  ends, so it only ever fires during genuine model silence. */
  private armFirstTokenWatchdog(): void {
    this.disarmFirstTokenWatchdog();
    this.firstTokenTimer = setTimeout(() => this.onFirstTokenTimeout(), FIRST_TOKEN_TIMEOUT_MS);
  }

  private disarmFirstTokenWatchdog(): void {
    if (this.firstTokenTimer !== null) {
      clearTimeout(this.firstTokenTimer);
      this.firstTokenTimer = null;
    }
  }

  private onFirstTokenTimeout(): void {
    this.firstTokenTimer = null;
    // Only act if we are still genuinely waiting for a first token (a late birth
    // or a finished/cancelled turn between scheduling and firing makes this a
    // no-op).
    if (!this.isActive || !this.awaitingFirstDelta) return;
    const streamId = this.streamId;
    this.markAbandoned(streamId);
    this.resetTTSPlayback();
    // recordError() calls finish(), which disarms; surface an actionable bubble
    // and stop the (still server-side) turn so the input unlocks for a retry.
    this.recordError(
      "server_unavailable",
      "The model took too long to respond and never started. It may be a very slow " +
        "model or an unresponsive provider. Try again, or pick a smaller local model.",
    );
    if (streamId) cores().api().chat.interrupt(streamId);
  }

  private onFrame(frame: ServerToClientFrame): void {
    if (frame.kind === "chat.message") {
      if (
        this.streamId === null &&
        !this.isActive &&
        frame.sessionId === (this.session?.id ?? null) &&
        !this.abandonedStreamIds.includes(frame.streamId)
      ) {
        // Core-initiated stream (a scheduled prompt or greeting fired) on
        // the session we're viewing: latch on at its first chat.message,
        // the only chat frame that carries sessionId. Deltas and the rest
        // then reduce through the normal paths below. A stream we just
        // abandoned is excluded so a late frame can't resurrect our own
        // interrupted bubble.
        this.adoptForeignStream(frame.streamId);
      }
      if (frame.streamId === this.streamId) {
        this.onChatMessage(frame.message as ServerMessage, frame.afterId, frame.final);
      }
      return;
    }
    if (frame.kind === "chat.delta" && frame.streamId === this.streamId) {
      const full = messagesState.appendDelta(frame.messageId, frame.delta);
      if (full !== null && frame.messageId === this.ttsTargetId) {
        this.feedTTS(full, false);
      }
      return;
    }
    if (frame.kind === "chat.usage" && frame.streamId === this.streamId) {
      messagesState.tokenUsage = frame.tokenUsage;
      return;
    }
    if (frame.kind === "chat.done" && frame.streamId === this.streamId) {
      this.finish();
      // Pops the window when this session was created with focus
      // "show_when_done" (no-op otherwise).
      if (this.session) this.session.notifyStreamDone(this.session.id);
      return;
    }
    if (frame.kind === "chat.error" && frame.streamId === this.streamId) {
      this.recordError(frame.code as LLMErrorType, frame.message);
      // A core-initiated turn that errors is still terminal: reveal a
      // "show_when_done" window now, exactly as chat.done does, so a failing
      // greeting on an autostart launch can't strand the app hidden forever.
      if (this.session) this.session.notifyStreamDone(this.session.id);
      return;
    }
    if (frame.kind === "schedule.confirm_request") {
      // Drives UserInput's schedule-confirm mode globally, same mechanism
      // as the permission mode below.
      scheduleConfirmState.set(frame);
      return;
    }
    if (
      frame.kind === "tool.progress" ||
      frame.kind === "tool.askuser_request" ||
      frame.kind === "tool.permission_request" ||
      frame.kind === "tool.log" ||
      frame.kind === "tool.result" ||
      frame.kind === "tool.error" ||
      frame.kind === "tool.cancelled"
    ) {
      if (frame.kind === "tool.permission_request") {
        // Drives UserInput's permission mode globally (the bubble state
        // alone can't: UserInput isn't per-message).
        permissionState.set(frame);
      } else if (
        frame.kind === "tool.result" ||
        frame.kind === "tool.error" ||
        frame.kind === "tool.cancelled"
      ) {
        // The call ended some other way (timeout, cancel, worker death)
        // while a request was still pending; drop it so the input returns
        // to normal.
        permissionState.clearForCall(frame.callId);
        scheduleConfirmState.clearForCall(frame.callId);
      }
      messagesState.applyToolEvent(frame);
    }
  }

  private onChatMessage(msg: ServerMessage, afterId: string | null, final: boolean): void {
    messagesState.applyServerMessage(msg, afterId);
    // The wire union widens into the client bag (every wire field is an
    // optional bag field); spread so the role-narrowed reads below are typed.
    const local: Message = { ...msg };
    if (local.role === "assistant" || local.role === "reasoning") {
      this.awaitingFirstDelta = false;
      // First token of this hop arrived: the model is responsive.
      this.disarmFirstTokenWatchdog();
    }
    if (!final) {
      const next = new Set(this.liveIds);
      next.add(msg.id);
      this.liveIds = next;
      if (local.role === "assistant") {
        this.claimTTS(msg.id);
        // Resume catch-up: a born snapshot already carrying content means we
        // re-attached mid-message (resubscribe). Advance the TTS cursor past
        // what already streamed so only new sentences are spoken, not the
        // whole message again.
        const existing = typeof local.content === "string" ? local.content : "";
        if (existing && msg.id === this.ttsTargetId) {
          let stripped = stripMarkdownForTTS(existing);
          if (!settingsState.currentSettings["tts.spellOutEmojis"]) {
            stripped = stripEmojisForTTS(stripped);
          }
          this.ttsCursor = stripped.length;
        }
      }
      return;
    }
    // Final snapshot.
    if (this.liveIds.has(msg.id)) {
      const next = new Set(this.liveIds);
      next.delete(msg.id);
      this.liveIds = next;
    }
    if (local.role === "assistant" && msg.id === this.ttsTargetId) {
      const text = typeof local.content === "string" ? local.content : "";
      this.feedTTS(text, true);
    }
    // A finished tool call means the next hop's prompt is processing; bring
    // the spinner back until that hop's first reasoning/assistant birth, and
    // re-arm the watchdog for that hop's first token.
    if (local.role === "tool" && this.isActive) {
      this.awaitingFirstDelta = true;
      this.armFirstTokenWatchdog();
    }
    // Core appended the tools hint to this turn's system prompt; mirror it
    // into the system bubble so it keeps showing exactly what the model
    // received.
    if (local.role === "tool_filter" && local.status === "complete" && (local.toolsSent ?? 0) > 0) {
      messagesState.appendSystemToolsHint(buildToolsHint());
    }
  }

  /** Latch onto a stream this client did not start. Mirrors start()'s state
   *  reset, minus the chat.start frame (core already runs the turn). */
  private adoptForeignStream(streamId: string): void {
    // A foreign (core-initiated) stream we latch onto at its first message is
    // already producing output; our own first-token watchdog doesn't apply.
    this.disarmFirstTokenWatchdog();
    this.isActive = true;
    this.awaitingFirstDelta = false;
    this.turnHadAssistant = false;
    this.liveIds = new Set();
    this.ttsTargetId = null;
    this.ttsCursor = 0;
    this.turnAnchorId = null;
    this.streamId = streamId;
  }

  /** Point the TTS feed at an assistant message. The turn's first assistant
   *  bubble starts the stream outright; later hops only claim the feed when
   *  playback is idle, so hop-1 speech is never cut mid-sentence. */
  private claimTTS(messageId: string): void {
    if (!this.turnHadAssistant) {
      this.turnHadAssistant = true;
      ttsState.startStream(messageId);
      this.ttsTargetId = messageId;
      this.ttsCursor = 0;
      return;
    }
    if (ttsState.liveSourceCount === 0 && !ttsState.synthInflight) {
      ttsState.currentMessageId = messageId;
      this.ttsTargetId = messageId;
      this.ttsCursor = 0;
    }
  }

  private feedTTS(fullText: string, final: boolean): void {
    const settings = settingsState.currentSettings;
    if (!settings["tts.enabled"]) return;
    if (!ttsState.loaded) return;
    if (
      ttsState.currentMessageId !== null &&
      this.ttsTargetId !== null &&
      ttsState.currentMessageId !== this.ttsTargetId
    ) {
      return;
    }
    let stripped = stripMarkdownForTTS(fullText);
    if (!settings["tts.spellOutEmojis"]) stripped = stripEmojisForTTS(stripped);
    // Speech belongs to the chat view: navigating away resets playback (see
    // viewState.navigate), and this guard keeps a still-running stream from
    // re-arming it at the next sentence boundary. The cursor skips what
    // streamed while away, so returning mid-stream resumes with new
    // sentences only.
    if (viewState.pendingMode !== "chat") {
      this.ttsCursor = stripped.length;
      return;
    }
    const remaining = stripped.slice(this.ttsCursor);
    if (!remaining) {
      if (final) ttsState.finalize();
      return;
    }
    const sentenceSeg = new Intl.Segmenter(undefined, {
      granularity: "sentence",
    });
    const sentences = Array.from(sentenceSeg.segment(remaining));
    const lastIdx = sentences.length - 1;
    for (let i = 0; i < sentences.length; i++) {
      const seg = sentences[i];
      const isTerminal = i < lastIdx || final;
      if (!isTerminal) break;
      const chunk = seg.segment.trim();
      if (chunk) ttsState.feedSentence(chunk);
      this.ttsCursor += seg.segment.length;
    }
    if (final) ttsState.finalize();
  }

  private finish(): void {
    this.disarmFirstTokenWatchdog();
    this.isActive = false;
    this.awaitingFirstDelta = false;
    this.streamId = null;
    this.turnAnchorId = null;
    this.liveIds = new Set();
  }

  /** Record a streamId we are walking away from so the adoption path won't
   *  re-latch onto its trailing frames. Bounded ring of recent ids. */
  private markAbandoned(streamId: string | null): void {
    if (!streamId) return;
    this.abandonedStreamIds = [...this.abandonedStreamIds, streamId].slice(-32);
  }

  recordError(errorType: LLMErrorType | ErrorCode, detail?: string): void {
    // Partial streamed content was already finalized server-side (flagged
    // interrupted), so the error gets its own bubble instead of replacing
    // anything.
    const content = detail ? `${errorType}\n${detail}` : String(errorType);
    messagesState.addMessage({ role: "error", content });
    this.finish();
  }

  /** Stop the turn from the user's side. The UI unlocks immediately;
   *  streamId stays set so the server's interrupted final snapshots (and the
   *  chat.done that follows) still land on the bubbles. */
  cancel(): void {
    if (!this.isActive) return;
    this.disarmFirstTokenWatchdog();
    this.resetTTSPlayback();
    this.isActive = false;
    this.awaitingFirstDelta = false;
  }

  /** Drop the local live-turn presentation when the connection goes away
   *  (transient drop or a deliberate core swap), WITHOUT abandoning or
   *  interrupting the stream. The turn keeps running server-side; the streamId
   *  is intentionally NOT marked abandoned so the resubscribe born snapshots
   *  can re-adopt it on reconnect for full live catch-up. */
  softReset(): void {
    this.resetTTSPlayback();
    this.finish();
  }

  /** Detach the current core's in-flight turn transparently: softReset plus
   *  clearing the open prompt INPUT MODES. Used on a transient transport drop
   *  AND on a deliberate core swap (where no "disconnected" is emitted, so the
   *  connection handler never fires and +page.svelte calls this directly before
   *  the session reload). Without this on a swap, sessions.load() would run
   *  interruptStreaming with the OLD core's stream still active, abandoning its
   *  streamId (blocking re-adoption on return) and firing a cross-core
   *  chat.interrupt. The turn keeps running server-side and is re-adopted via
   *  the resubscribe born snapshots on reconnect / when we swap back. */
  detachForResume(): void {
    if (this.isActive) log.info("stream detached: resuming on reconnect / return");
    this.softReset();
    // The pending permission / schedule-confirm INPUT MODES are core-specific
    // and would post into a dropped (or now-foreign) socket, so clear them; the
    // tool bubble itself stays awaiting and the prompt is re-emitted on
    // resubscribe. The askUser form lives on the message ephemera and is
    // likewise restored by the resubscribe re-emit after the reload.
    permissionState.clear();
    scheduleConfirmState.clear();
  }

  abortSilently(): void {
    if (!this.isActive) return;
    this.resetTTSPlayback();
    const streamId = this.streamId;
    this.markAbandoned(streamId);
    this.finish();
    if (streamId) cores().api().chat.interrupt(streamId);
  }

  async interruptStreaming(): Promise<void> {
    if (!this.isActive && !messagesState.hasActiveToolCall) return;
    const streamId = this.streamId;
    this.markAbandoned(streamId);
    this.cancel();
    await Promise.all(this.interruptListeners.map((fn) => fn()));
    if (streamId) cores().api().chat.interrupt(streamId);
  }

  resetForSession(): void {
    this.disarmFirstTokenWatchdog();
    this.isActive = false;
    this.awaitingFirstDelta = false;
    this.streamId = null;
    this.turnAnchorId = null;
    this.liveIds = new Set();
    this.ttsTargetId = null;
    this.ttsCursor = 0;
    this.turnHadAssistant = false;
  }
}

export const streamingState = new StreamingState();

if (import.meta.hot) {
  // Dev-only. HMR re-evaluates this module and replaces `streamingState` with a
  // fresh instance, but the prior instance's WS subscription lives in the
  // cores() singleton (not re-evaluated here), so it stays bound to the live
  // socket. Left attached, every hot-replaced instance keeps receiving frames;
  // sitting at streamId=null/isActive=false it adopts the next user stream via
  // the foreign-stream path and re-feeds the same assistant text into the
  // shared TTS queue. Each accumulated instance adds one more pass, so a
  // streamed sentence is synthesized and played once per instance (the
  // "sentences/batches repeat 2-4x after a few saves" bug, dev only). Detach
  // the outgoing instance before it's discarded so only the live one feeds TTS.
  import.meta.hot.dispose(() => streamingState.detach());
}
