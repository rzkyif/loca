// Streaming reducer tests.
//
// Disconnect recovery: a core hot-reload drops the WS without a terminal
// chat.done/chat.error frame, so streamingState must clear isActive itself or
// the spinner spins forever. We mock $lib/core to capture the listeners
// attach() registers, then drive transitions/frames by hand.
//
// Live == reload parity: the single most valuable invariant of the
// server-authoritative protocol. A recorded two-hop frame script driven
// through onFrame must leave messagesState in exactly the order a reload of
// the same persisted messages produces.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Message as ServerMessage, ServerToClientFrame } from "@loca/shared";

const hoisted = vi.hoisted(() => ({
  connListener: null as null | ((state: string, reason?: string) => void),
  wsListener: null as null | ((frame: unknown) => void),
  started: [] as Array<Record<string, unknown>>,
  interrupts: [] as string[],
}));

vi.mock("$lib/core", () => ({
  cores: () => ({
    subscribeWs: (l: (frame: unknown) => void) => {
      hoisted.wsListener = l;
      return () => {};
    },
    subscribeConnectionState: (l: (state: string, reason?: string) => void) => {
      hoisted.connListener = l;
      return () => {};
    },
    api: () => ({
      chat: {
        interrupt: (streamId: string) => {
          hoisted.interrupts.push(streamId);
        },
        start: (
          streamId: string,
          sessionId: string,
          route: string,
          opts?: Record<string, unknown>,
        ) => {
          hoisted.started.push({ streamId, sessionId, route, ...opts });
        },
      },
    }),
  }),
}));

import { streamingState } from "./streaming.svelte";
import { messagesState } from "./messages.svelte";
import { fixupLoadedMessages, sessionsState } from "./sessions.svelte";
// Wire the store ports (messages<->streaming, sessions<->streaming) the same way
// the app does, so streamingState reads the session id through its injected port.
import "./store-wiring";

describe("streamingState disconnect recovery", () => {
  beforeEach(() => {
    hoisted.connListener = null;
    streamingState.detach();
    streamingState.resetForSession();
    messagesState.clear();
  });
  afterEach(() => streamingState.detach());

  it("clears isActive when the connection drops mid-stream", () => {
    streamingState.attach();
    expect(hoisted.connListener).toBeTypeOf("function");
    streamingState.isActive = true;
    hoisted.connListener?.("disconnected");
    expect(streamingState.isActive).toBe(false);
  });

  it("does nothing on a connected transition", () => {
    streamingState.attach();
    streamingState.isActive = true;
    hoisted.connListener?.("connected");
    expect(streamingState.isActive).toBe(true);
  });
});

// A deliberate core swap closes the old socket WITHOUT a "disconnected" emit, so
// the connection handler above never fires; +page.svelte calls detachForResume()
// directly. It must drop the local turn WITHOUT abandoning the streamId or
// interrupting the still-running turn, so swapping back can re-adopt it. The old
// bug let sessions.load()'s interruptStreaming run with the turn still active,
// which abandoned the id (blocking re-adoption) and cross-interrupted.
describe("streamingState core swap teardown", () => {
  beforeEach(() => {
    hoisted.connListener = null;
    hoisted.wsListener = null;
    hoisted.started = [];
    hoisted.interrupts = [];
    streamingState.detach();
    streamingState.resetForSession();
    messagesState.clear();
    sessionsState.id = "s1";
  });
  afterEach(() => {
    streamingState.detach();
    sessionsState.id = null;
  });

  it("detaches an in-flight turn without abandoning its streamId or interrupting", () => {
    streamingState.attach();
    messagesState.hydrate([{ id: "u1", role: "user", content: "hi" }], null);
    streamingState.beginTurn(null);
    streamingState.start();
    const streamIdA = streamingState.streamId!;
    expect(streamingState.isActive).toBe(true);

    streamingState.detachForResume();
    expect(streamingState.isActive).toBe(false);
    expect(streamingState.streamId).toBeNull();
    // The swap must NOT interrupt the still-running turn on core A.
    expect(hoisted.interrupts).toEqual([]);

    // Swapping back to A and reopening s1: the core re-emits the born snapshot
    // with the SAME streamId. It must re-adopt (the id was not abandoned), so
    // the live tail resumes.
    const born = {
      kind: "chat.message",
      streamId: streamIdA,
      sessionId: "s1",
      message: { id: "a1", role: "assistant", content: "Hello" },
      afterId: "u1",
      final: false,
    } as unknown as ServerToClientFrame;
    hoisted.wsListener?.(born);
    expect(streamingState.streamId).toBe(streamIdA);
    expect(streamingState.isActive).toBe(true);
    expect(messagesState.messages.find((m) => m.id === "a1")?.content).toBe("Hello");
  });

  it("a user interrupt still abandons the streamId so trailing frames can't re-adopt", async () => {
    streamingState.attach();
    messagesState.hydrate([{ id: "u1", role: "user", content: "hi" }], null);
    streamingState.beginTurn(null);
    streamingState.start();
    const streamIdA = streamingState.streamId!;

    await streamingState.interruptStreaming();
    expect(hoisted.interrupts).toEqual([streamIdA]);

    // The session boundary that follows a user interrupt clears streamId; a late
    // born frame for the abandoned stream must NOT resurrect its bubble.
    streamingState.resetForSession();
    const born = {
      kind: "chat.message",
      streamId: streamIdA,
      sessionId: "s1",
      message: { id: "a1", role: "assistant", content: "Hello" },
      afterId: "u1",
      final: false,
    } as unknown as ServerToClientFrame;
    hoisted.wsListener?.(born);
    expect(streamingState.streamId).toBeNull();
    expect(streamingState.isActive).toBe(false);
    expect(messagesState.messages.find((m) => m.id === "a1")).toBeUndefined();
  });
});

describe("streamingState live == reload parity", () => {
  beforeEach(() => {
    hoisted.wsListener = null;
    hoisted.started = [];
    streamingState.detach();
    streamingState.resetForSession();
    messagesState.clear();
    sessionsState.id = "s1";
  });
  afterEach(() => {
    streamingState.detach();
    sessionsState.id = null;
  });

  it("a two-hop tool turn lands in the same order live as on reload", () => {
    streamingState.attach();
    messagesState.hydrate([{ id: "u1", role: "user", content: "Open YouTube" }], null);
    streamingState.beginTurn(null);
    streamingState.start();
    const streamId = streamingState.streamId!;
    expect(hoisted.started).toHaveLength(1);

    // Persisted terminal forms, oldest-first, as GET /sessions/:id returns.
    const persisted: ServerMessage[] = [
      { id: "u1", role: "user", content: "Open YouTube" },
      {
        id: "f1",
        role: "tool_filter",
        status: "complete",
        phase1: [],
        toolsSent: 1,
      },
      {
        id: "r1",
        role: "reasoning",
        content: "Thinking.",
        reasoningDurationMs: 5,
        pairedAssistantId: "a1",
      },
      { id: "a1", role: "assistant", content: "Opening." },
      {
        id: "t1",
        role: "tool",
        callId: "c1",
        extensionId: "kit",
        toolName: "open_website",
        arguments: '{"url":"https://youtube.com"}',
        status: "completed",
        result: { ok: true },
      },
      {
        id: "r2",
        role: "reasoning",
        content: "Done thinking.",
        pairedAssistantId: "a2",
      },
      { id: "a2", role: "assistant", content: "Done." },
    ] as unknown as ServerMessage[];
    const byId = new Map(persisted.map((m) => [(m as { id: string }).id, m]));

    const birth = (id: string, afterId: string | null, patch: Record<string, unknown>) =>
      ({
        kind: "chat.message",
        streamId,
        sessionId: "s1",
        message: { ...(byId.get(id) as object), ...patch },
        afterId,
        final: false,
      }) as unknown as ServerToClientFrame;
    const final = (id: string, afterId: string | null = null) =>
      ({
        kind: "chat.message",
        streamId,
        sessionId: "s1",
        message: byId.get(id),
        afterId,
        final: true,
      }) as ServerToClientFrame;
    const delta = (messageId: string, text: string) =>
      ({
        kind: "chat.delta",
        streamId,
        messageId,
        delta: text,
      }) as ServerToClientFrame;

    const script: ServerToClientFrame[] = [
      // The tool_filter bubble has no loading state: it emits only its terminal
      // snapshot (first-emission finalize), positioned right after the user msg.
      final("f1", "u1"),
      birth("r1", "f1", { content: "", reasoningDurationMs: undefined }),
      delta("r1", "Thinking."),
      birth("a1", "r1", { content: "" }),
      final("r1"),
      delta("a1", "Opening."),
      final("a1"),
      birth("t1", "a1", { status: "running", result: undefined }),
      final("t1"),
      birth("r2", "t1", { content: "" }),
      delta("r2", "Done thinking."),
      birth("a2", "r2", { content: "" }),
      final("r2"),
      delta("a2", "Done."),
      final("a2"),
      { kind: "chat.done", streamId, reason: "stop" },
    ];
    for (const frame of script) hoisted.wsListener?.(frame);

    expect(streamingState.isActive).toBe(false);
    // The system bubble is client-local (the tools-hint mirror creates it
    // when toolsSent > 0) and never persisted, so it sits outside the
    // parity comparison.
    const liveIds = messagesState.messages.filter((m) => m.role !== "system").map((m) => m.id);
    const reloadIds = fixupLoadedMessages(persisted)
      .reverse()
      .map((m) => m.id);
    expect(liveIds).toEqual(reloadIds);
    // Content converged too, not just order.
    expect(messagesState.messages.find((m) => m.id === "a2")?.content).toBe("Done.");
    expect(messagesState.messages.find((m) => m.id === "t1")?.status).toBe("completed");
  });
});

// First-token watchdog: a provider that accepts the request but never streams a
// token would spin the loading sentinel forever. The watchdog surfaces an error
// and interrupts the turn, but only while genuinely awaiting the first token.
describe("streamingState first-token watchdog", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    hoisted.wsListener = null;
    hoisted.started = [];
    hoisted.interrupts = [];
    streamingState.detach();
    streamingState.resetForSession();
    messagesState.clear();
    sessionsState.id = "s1";
  });
  afterEach(() => {
    streamingState.detach();
    streamingState.resetForSession();
    sessionsState.id = null;
    vi.useRealTimers();
  });

  it("errors and interrupts when no first token ever arrives", () => {
    streamingState.attach();
    messagesState.hydrate([{ id: "u1", role: "user", content: "hi" }], null);
    streamingState.beginTurn(null);
    streamingState.start();
    const streamId = streamingState.streamId!;
    expect(streamingState.awaitingFirstDelta).toBe(true);

    vi.advanceTimersByTime(120_000);

    expect(streamingState.isActive).toBe(false);
    expect(hoisted.interrupts).toEqual([streamId]);
    const err = messagesState.messages.find((m) => m.role === "error");
    expect(typeof err?.content === "string" && err.content.includes("too long")).toBe(true);
  });

  it("does not fire once the first token has arrived", () => {
    streamingState.attach();
    messagesState.hydrate([{ id: "u1", role: "user", content: "hi" }], null);
    streamingState.beginTurn(null);
    streamingState.start();
    const streamId = streamingState.streamId!;

    const born = {
      kind: "chat.message",
      streamId,
      sessionId: "s1",
      message: { id: "a1", role: "assistant", content: "" },
      afterId: "u1",
      final: false,
    } as unknown as ServerToClientFrame;
    hoisted.wsListener?.(born);
    expect(streamingState.awaitingFirstDelta).toBe(false);

    vi.advanceTimersByTime(120_000);

    // The watchdog was disarmed by the birth: no error, no interrupt, turn lives.
    expect(messagesState.messages.find((m) => m.role === "error")).toBeUndefined();
    expect(hoisted.interrupts).toEqual([]);
    expect(streamingState.isActive).toBe(true);
  });
});
