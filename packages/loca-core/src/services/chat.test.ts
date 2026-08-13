// chat orchestrator integration tests. Drives turns end-to-end through the
// public seams:
//
//   HTTP /pairing → HTTP /sessions → HTTP POST /messages
//     ↓
//   WS /ws/v1 chat.start
//     ↓
//   chat.ts emits chat.message births/finals + chat.delta → wsHub broadcasts
//     ↓
//   collect frames; assert the protocol invariants AND that the final
//   snapshots converge with what GET /sessions/:id persists (live == reload).
//
// The OpenAI SDK is intercepted at globalThis.fetch. `resolveEndpoint()`
// doesn't inject a per-config fetch, but the SDK reads global fetch
// dynamically, so swapping it for the duration of the test is enough.

import { assertEquals } from "@std/assert";
import { pairClient } from "../../tests/helpers/pairing.ts";
import { startTestServer } from "../../tests/helpers/serve.ts";
import { dialWs } from "../../tests/helpers/ws.ts";
import { patchCoreSettings } from "@loca/core-engine/services/core-settings";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { fakeOpenAIFetch } from "../../tests/helpers/fake-openai.ts";

interface CollectedFrame {
  kind: string;
  // deno-lint-ignore no-explicit-any
  [k: string]: any;
}

function waitForFrame(
  ws: WebSocket,
  predicate: (frame: CollectedFrame) => boolean,
  timeoutMs = 5_000,
): Promise<{ matched: CollectedFrame; all: CollectedFrame[] }> {
  const all: CollectedFrame[] = [];
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(
        new Error(
          `timeout waiting for frame; saw ${all.length}: ${all.map((f) => f.kind).join(",")}`,
        ),
      );
    }, timeoutMs);
    const onMsg = (ev: MessageEvent) => {
      if (typeof ev.data !== "string") return;
      const frame = JSON.parse(ev.data) as CollectedFrame;
      all.push(frame);
      if (predicate(frame)) {
        cleanup();
        resolve({ matched: frame, all });
      }
    };
    const cleanup = () => {
      clearTimeout(timer);
      ws.removeEventListener("message", onMsg);
    };
    ws.addEventListener("message", onMsg);
  });
}

// Shared per-test scaffold: env + server + fetch interception routed by URL
// prefix (only the fake OpenAI endpoint is intercepted; loopback requests to
// the in-process test server pass through).
async function withChatHarness(
  openaiFetch: typeof fetch,
  fn: (h: {
    port: number;
    token: string;
    api: (path: string, init?: RequestInit) => Promise<Response>;
    openWs: () => Promise<WebSocket>;
  }) => Promise<void>,
): Promise<void> {
  const env = await setupTestEnv();
  const server = await startTestServer();
  const originalFetch = globalThis.fetch;
  globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof URL
          ? input.toString()
          : (input as Request).url;
    if (url.startsWith("https://stub.test/")) return openaiFetch(input, init);
    return originalFetch(input, init);
  }) as typeof fetch;
  const sockets: WebSocket[] = [];
  try {
    await patchCoreSettings({
      "llm.provider": "external",
      "llm.external.baseUrl": "https://stub.test/v1",
      "llm.external.apiKey": "sk-test",
      "llm.external.model": "test-model",
    });
    const { token } = await pairClient("chat-t2", "127.0.0.1");
    const api = (path: string, init?: RequestInit) =>
      fetch(`http://127.0.0.1:${server.port}/api/v1${path}`, {
        ...init,
        headers: {
          authorization: `Bearer ${token}`,
          "content-type": "application/json",
          ...init?.headers,
        },
      });
    const openWs = async () => {
      const ws = await dialWs(server.port, token);
      sockets.push(ws);
      await new Promise<void>((resolve, reject) => {
        ws.addEventListener("open", () => resolve(), { once: true });
        ws.addEventListener("error", reject, { once: true });
      });
      return ws;
    };
    await fn({ port: server.port, token, api, openWs });
  } finally {
    for (const ws of sockets) {
      try {
        ws.close();
      } catch {
        /* already closed */
      }
    }
    globalThis.fetch = originalFetch;
    await server.stop();
    await env.teardown();
  }
}

async function createSessionWithUserMessage(
  api: (path: string, init?: RequestInit) => Promise<Response>,
  text: string,
): Promise<{ sessionId: string; userId: string }> {
  const created = await api("/sessions", {
    method: "POST",
    body: JSON.stringify({ title: "" }),
  });
  const session = await created.json();
  const posted = await api(`/sessions/${session.id}/messages`, {
    method: "POST",
    body: JSON.stringify({ role: "user", content: text }),
  });
  const userMsg = await posted.json();
  return { sessionId: session.id, userId: userMsg.id };
}

async function listPersisted(
  api: (path: string, init?: RequestInit) => Promise<Response>,
  sessionId: string,
): Promise<CollectedFrame[]> {
  const res = await api(`/sessions/${sessionId}`);
  const full = await res.json();
  return full.messages as CollectedFrame[];
}

Deno.test({
  name: "chat.start happy path: birth before delta, final converges with persistence",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    await withChatHarness(fakeOpenAIFetch("streaming-basic.sse"), async ({ api, openWs }) => {
      const { sessionId, userId } = await createSessionWithUserMessage(api, "hi");
      const ws = await openWs();
      const done = waitForFrame(ws, (f) => f.kind === "chat.done");
      ws.send(
        JSON.stringify({
          kind: "chat.start",
          streamId: "s-1",
          sessionId,
          route: "default",
        }),
      );
      const { matched, all } = await done;
      assertEquals(matched.reason, "stop");
      assertEquals(all.filter((f) => f.kind === "chat.error").length, 0);

      const births = all.filter((f) => f.kind === "chat.message" && !f.final);
      const finals = all.filter((f) => f.kind === "chat.message" && f.final);
      const deltas = all.filter((f) => f.kind === "chat.delta");
      assertEquals(births.length, 1);
      assertEquals(births[0].message.role, "assistant");
      assertEquals(births[0].message.content, "");
      assertEquals(births[0].afterId, userId);
      // Birth precedes the first delta for that id.
      assertEquals(
        all.findIndex((f) => f.kind === "chat.message" && !f.final) <
          all.findIndex((f) => f.kind === "chat.delta"),
        true,
      );
      assertEquals(deltas.map((d) => d.delta).join(""), "Hello world");
      assertEquals(finals.length, 1);
      assertEquals(finals[0].message.content, "Hello world");
      assertEquals(finals[0].message.id, births[0].message.id);

      // Convergence: the persisted session equals the final snapshots.
      const persisted = await listPersisted(api, sessionId);
      assertEquals(
        persisted.map((m) => m.id),
        [userId, finals[0].message.id],
      );
      assertEquals(persisted[1].content, "Hello world");
    });
  },
});

Deno.test({
  name: "chat.start: an unconfigured external provider fails fast with an actionable error",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    await withChatHarness(fakeOpenAIFetch("streaming-basic.sse"), async ({ api, openWs }) => {
      // Blank out the external config the harness seeded: the user picked
      // External but hasn't filled in the provider details yet.
      await patchCoreSettings({
        "llm.external.baseUrl": "",
        "llm.external.apiKey": "",
        "llm.external.model": "",
      });
      const { sessionId } = await createSessionWithUserMessage(api, "hi");
      const ws = await openWs();
      const err = waitForFrame(ws, (f) => f.kind === "chat.error");
      ws.send(
        JSON.stringify({ kind: "chat.start", streamId: "s-err", sessionId, route: "default" }),
      );
      const { matched, all } = await err;
      assertEquals(matched.code, "provider_error");
      // Actionable: it points the user at Settings, not an opaque SDK failure.
      assertEquals(matched.message.includes("set up"), true);
      // The fake OpenAI endpoint is never hit and nothing streams.
      assertEquals(all.filter((f) => f.kind === "chat.delta").length, 0);
    });
  },
});

Deno.test({
  name: "two-hop tool turn: births/finals in order, persisted order identical",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    // Hop 1 streams reasoning + a tool call (unknown tool, so execution
    // fails fast without a extension), hop 2 streams the plain answer.
    let call = 0;
    const seqFetch: typeof fetch = (input, init) => {
      const fixture = call++ === 0 ? "streaming-reasoning-tools.sse" : "streaming-basic.sse";
      return fakeOpenAIFetch(fixture)(input, init);
    };
    await withChatHarness(seqFetch, async ({ api, openWs }) => {
      const { sessionId, userId } = await createSessionWithUserMessage(api, "do the thing");
      const ws = await openWs();
      const done = waitForFrame(ws, (f) => f.kind === "chat.done");
      ws.send(
        JSON.stringify({
          kind: "chat.start",
          streamId: "s-2",
          sessionId,
          route: "default",
        }),
      );
      const { matched, all } = await done;
      assertEquals(matched.reason, "stop");

      const finals = all.filter((f) => f.kind === "chat.message" && f.final);
      assertEquals(
        finals.map((f) => f.message.role),
        ["reasoning", "assistant", "tool", "assistant"],
      );
      const [reasoning, assistant1, tool, assistant2] = finals.map((f) => f.message);
      assertEquals(reasoning.content, "Let me think... about this.");
      assertEquals(reasoning.pairedAssistantId, assistant1.id);
      // The tool-call-only assistant persists the model's tool_calls.
      assertEquals(assistant1.content, "");
      assertEquals(assistant1.toolCalls.length, 1);
      assertEquals(assistant1.toolCalls[0].toolName, "do_thing");
      // The correlation id is namespaced by streamId (and tool-call index) so
      // concurrent turns reusing the model's "call_1" can't collide; the
      // assistant tool_call and its tool result carry the SAME namespaced id so
      // the model replay stays consistent.
      assertEquals(tool.callId, "s-2:0:call_1");
      assertEquals(assistant1.toolCalls[0].callId, tool.callId);
      assertEquals(tool.arguments, '{"x":42}');
      assertEquals(tool.status, "failed");
      assertEquals(assistant2.content, "Hello world");

      // Birth positions chain the turn together.
      const birthAfter = new Map(
        all
          .filter((f) => f.kind === "chat.message" && !f.final)
          .map((f) => [f.message.id, f.afterId]),
      );
      assertEquals(birthAfter.get(reasoning.id), userId);
      assertEquals(birthAfter.get(tool.id), assistant1.id);

      // live == reload: persisted order equals the finals' emission order.
      const persisted = await listPersisted(api, sessionId);
      assertEquals(
        persisted.map((m) => m.id),
        [userId, reasoning.id, assistant1.id, tool.id, assistant2.id],
      );
      assertEquals(
        persisted.map((m) => m.ord),
        [0, 1, 2, 3, 4],
      );
    });
  },
});

Deno.test({
  name: "anchored regenerate: deletes the old turn, truncates the transcript, inserts mid-history",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    const requestBodies: string[] = [];
    const recordingFetch: typeof fetch = (input, init) => {
      requestBodies.push(String(init?.body ?? ""));
      return fakeOpenAIFetch("streaming-basic.sse")(input, init);
    };
    await withChatHarness(recordingFetch, async ({ api, openWs }) => {
      const { sessionId, userId: u1 } = await createSessionWithUserMessage(api, "turn one");
      const seed = async (role: string, content: string) => {
        const res = await api(`/sessions/${sessionId}/messages`, {
          method: "POST",
          body: JSON.stringify({ role, content }),
        });
        return (await res.json()).id as string;
      };
      const a1 = await seed("assistant", "reply one");
      const u2 = await seed("user", "turn two");
      const a2 = await seed("assistant", "reply two");

      const ws = await openWs();
      const done = waitForFrame(ws, (f) => f.kind === "chat.done");
      ws.send(
        JSON.stringify({
          kind: "chat.start",
          streamId: "s-3",
          sessionId,
          route: "default",
          anchorMessageId: u1,
        }),
      );
      const { all } = await done;

      // The old turn's reply was deleted server-side and announced.
      const deleted = all.filter((f) => f.kind === "session.updated" && f.op === "message_deleted");
      assertEquals(
        deleted.map((f) => f.payload.messageId),
        [a1],
      );

      // The outbound transcript stopped at the anchor (inclusive). Index 0 is
      // the turn's completion request; the fire-and-forget title generation in
      // onStop issues a later request that carries the full recent transcript.
      const sent = requestBodies[0];
      assertEquals(sent.includes("turn one"), true);
      assertEquals(sent.includes("turn two"), false);
      assertEquals(sent.includes("reply two"), false);

      // The new reply persisted INTO the anchor's slot, between the turns.
      const finals = all.filter((f) => f.kind === "chat.message" && f.final);
      assertEquals(finals.length, 1);
      const persisted = await listPersisted(api, sessionId);
      assertEquals(
        persisted.map((m) => m.id),
        [u1, finals[0].message.id, u2, a2],
      );
    });
  },
});

Deno.test({
  name: "interrupt mid-stream: partial persisted with interrupted flag, chat.done interrupted",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    // Streams one content chunk and then stalls until the request is
    // aborted; the abort errors the body stream like a real connection.
    const encoder = new TextEncoder();
    const head =
      'data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"t","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}\n\n';
    const gatedFetch: typeof fetch = (_input, init) => {
      const signal = init?.signal as AbortSignal | undefined;
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(encoder.encode(head));
          signal?.addEventListener("abort", () => {
            try {
              controller.error(new DOMException("aborted", "AbortError"));
            } catch {
              /* already closed */
            }
          });
        },
      });
      return Promise.resolve(
        new Response(body, {
          status: 200,
          headers: {
            "content-type": "text/event-stream",
            "cache-control": "no-cache",
          },
        }),
      );
    };
    await withChatHarness(gatedFetch, async ({ api, openWs }) => {
      const { sessionId, userId } = await createSessionWithUserMessage(api, "hi");
      const ws = await openWs();
      const done = waitForFrame(ws, (f) => f.kind === "chat.done", 10_000);
      const firstDelta = waitForFrame(ws, (f) => f.kind === "chat.delta");
      ws.send(
        JSON.stringify({
          kind: "chat.start",
          streamId: "s-4",
          sessionId,
          route: "default",
        }),
      );
      await firstDelta;
      ws.send(JSON.stringify({ kind: "chat.interrupt", streamId: "s-4" }));
      const { matched, all } = await done;
      assertEquals(matched.reason, "interrupted");
      assertEquals(all.filter((f) => f.kind === "chat.error").length, 0);

      const finals = all.filter((f) => f.kind === "chat.message" && f.final);
      assertEquals(finals.length, 1);
      assertEquals(finals[0].message.content, "Hello");
      assertEquals(finals[0].message.interrupted, true);

      const persisted = await listPersisted(api, sessionId);
      assertEquals(
        persisted.map((m) => m.id),
        [userId, finals[0].message.id],
      );
      assertEquals(persisted[1].interrupted, true);
      assertEquals(persisted[1].content, "Hello");
    });
  },
});

Deno.test({
  name: "resubscribe mid-stream: reconnecting client gets catch-up snapshot then live tail",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    // Streams "Hello", then holds the body open until the test releases the
    // tail (" world" + stop + [DONE]), so the turn is genuinely in-flight while
    // the client swaps sockets.
    const encoder = new TextEncoder();
    const head =
      'data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"t","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}\n\n';
    const tail =
      'data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"t","choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}\n\n' +
      'data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"t","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\n' +
      "data: [DONE]\n\n";
    let tailController: ReadableStreamDefaultController<Uint8Array> | null = null;
    const gatedFetch: typeof fetch = (_input, _init) => {
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(encoder.encode(head));
          tailController = controller;
        },
      });
      return Promise.resolve(
        new Response(body, {
          status: 200,
          headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
        }),
      );
    };
    await withChatHarness(gatedFetch, async ({ api, openWs }) => {
      const { sessionId, userId } = await createSessionWithUserMessage(api, "hi");
      const ws1 = await openWs();
      const firstDelta = waitForFrame(ws1, (f) => f.kind === "chat.delta");
      ws1.send(
        JSON.stringify({ kind: "chat.start", streamId: "s-5", sessionId, route: "default" }),
      );
      await firstDelta;

      // Client swaps away: drop ws1 mid-turn. Generation continues server-side.
      ws1.close();

      // Reconnect as the same client and resubscribe.
      const ws2 = await openWs();
      const born = waitForFrame(
        ws2,
        (f) => f.kind === "chat.message" && !f.final && f.streamId === "s-5",
      );
      ws2.send(JSON.stringify({ kind: "chat.subscribe", sessionId }));
      const { matched: bornFrame } = await born;
      // Catch-up snapshot carries the partial content streamed before reconnect.
      assertEquals(bornFrame.message.role, "assistant");
      assertEquals(bornFrame.message.content, "Hello");
      assertEquals(bornFrame.afterId, userId);

      // Release the tail; ws2 (same clientId) receives the live remainder.
      const done = waitForFrame(ws2, (f) => f.kind === "chat.done");
      tailController!.enqueue(encoder.encode(tail));
      tailController!.close();
      const { all } = await done;

      const tailDeltas = all.filter((f) => f.kind === "chat.delta" && f.streamId === "s-5");
      assertEquals(tailDeltas.map((d) => d.delta).join(""), " world");
      const finals = all.filter((f) => f.kind === "chat.message" && f.final);
      assertEquals(finals.at(-1)!.message.content, "Hello world");

      // Convergence: persisted == final snapshot.
      const persisted = await listPersisted(api, sessionId);
      assertEquals(
        persisted.map((m) => m.id),
        [userId, finals.at(-1)!.message.id],
      );
      assertEquals(persisted.at(-1)!.content, "Hello world");
    });
  },
});

Deno.test({
  name: "resubscribe is owner-scoped: a different client gets no catch-up frames",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    const encoder = new TextEncoder();
    const head =
      'data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"t","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}\n\n';
    let tailController: ReadableStreamDefaultController<Uint8Array> | null = null;
    const gatedFetch: typeof fetch = (_input, _init) => {
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(encoder.encode(head));
          tailController = controller;
        },
      });
      return Promise.resolve(
        new Response(body, {
          status: 200,
          headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
        }),
      );
    };
    await withChatHarness(gatedFetch, async ({ port, api, openWs }) => {
      const { sessionId } = await createSessionWithUserMessage(api, "hi");
      const ws1 = await openWs();
      const firstDelta = waitForFrame(ws1, (f) => f.kind === "chat.delta");
      ws1.send(
        JSON.stringify({ kind: "chat.start", streamId: "s-6", sessionId, route: "default" }),
      );
      await firstDelta;

      // A SECOND, distinct client pairs and subscribes to the same session id.
      // It does not own the stream (and can't even see the session), so it must
      // receive nothing.
      const { token: otherToken } = await pairClient("chat-other", "127.0.0.1");
      const ws2 = await dialWs(port, otherToken);
      await new Promise<void>((resolve, reject) => {
        ws2.addEventListener("open", () => resolve(), { once: true });
        ws2.addEventListener("error", reject, { once: true });
      });
      const got: string[] = [];
      ws2.addEventListener("message", (ev) => {
        if (typeof ev.data === "string") got.push(JSON.parse(ev.data).kind);
      });
      ws2.send(JSON.stringify({ kind: "chat.subscribe", sessionId }));
      // Give the server a beat to (not) emit anything.
      await new Promise((r) => setTimeout(r, 150));
      assertEquals(
        got.filter((k) => k === "chat.message"),
        [],
      );
      ws2.close();
      tailController!.error(new DOMException("done", "AbortError"));
      ws1.send(JSON.stringify({ kind: "chat.interrupt", streamId: "s-6" }));
    });
  },
});
