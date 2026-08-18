// D4: the chat orchestrator.
//
// D2's file states that `POST /:id/chat` lands here rather than there
// (`d2-sessions.ts:11-13`), and the WS turn has no other home either: it is the
// only wire surface D4 owns, because every HTTP route that reaches chat is D2's
// and goes through the `ChatEngine` seam.
//
// ## What makes these deterministic
//
// Every seeded core is `llm.provider = "external"` pointed at the loopback mock
// (`harness/home.ts`). Its default `echo` script streams the LAST message's
// content back, which is stronger than it sounds on a multi-hop turn: after a
// tool hop the last message is the TOOL RESULT, so the text can only appear in
// the reply if the core really dispatched the call and fed its result back in
// the right place. A core that dropped the result, or put it in the wrong slot,
// produces different text rather than the same text more slowly.
//
// The mock's opening `{role:"assistant",content:""}` chunk and its closing
// `finish_reason` chunk both carry no text, and BOTH cores drop an empty content
// delta rather than emitting a blank frame, which is why a one-sentence reply is
// one `chat.delta` and not three.
//
// ## Frame matching
//
// A `ws` step matches an ORDERED SUBSEQUENCE filtered to the kinds it names, so
// naming `chat.message` and `chat.done` ignores the `chat.delta` traffic and the
// heartbeat between them. That is what keeps these stable; it also means adding
// a kind to an `expect` list tightens the case, because every frame of that kind
// then has to match in order.
//
// ## What these deliberately do NOT cover, and why
//
//   - A REAL tool executing. The harness boots no extension subsystem and no MCP
//     server, so every scripted tool call resolves as unknown. That still
//     exercises assembly, the id join, the failure fold and the hop feed (see
//     `two-hop` below), but the worker path itself is Rust-side only.
//   - `session_busy`. Reaching it needs a second `chat.start` while the first
//     turn is still running, and the mock answers in milliseconds, so a case
//     asserting it would be racing by construction rather than by accident.
//     Covered in Rust against a provider that does not finish.
//   - The `chat.interrupt` cross-client ownership guard. A `ws` step authenticates
//     as the case's primary client only, so a second client's socket is not
//     expressible. Covered in Rust.
//   - Interrupting mid-stream, and the partial-message persistence that follows.
//     Same reason as `session_busy`: there is no window.
//   - The provider-error mapping. The `error` script is installed per case, so a
//     case could reach it, but it asserts a code mapping that is already pinned
//     by Rust tests over `classify_provider_error`; the wire adds nothing.

import type { Case } from "../runner/types.ts";

export const cases: Case[] = [
  {
    id: "d4.chat.sse-fallback-streams-and-persists-the-turn",
    package: "D4",
    routes: ["POST /api/v1/sessions", "POST /api/v1/sessions/:id/chat", "GET /api/v1/sessions/:id"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "sse" } },
        expect: { status: 200, bodyContains: { id: "<id>" } },
        capture: { sid: "$.id" },
      },
      {
        // The route persists the user message BEFORE generating, so the
        // transcript below proves ordering as well as content.
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/chat",
          body: { content: "hello sse" },
        },
        expect: {
          status: 200,
          // `usage` is absent rather than null on the terminal line: the mock
          // sends no usage chunk, and both cores omit the key entirely. A core
          // that emitted `"usage":null` would fail here, which is the point.
          sse: [{ contentDelta: "hello sse" }, { done: true }],
        },
      },
      {
        // The generation is only half the contract. The other half is that the
        // turn is reloadable: user then assistant, in that order, with the
        // assistant carrying exactly what was streamed.
        request: { method: "GET", path: "/api/v1/sessions/${sid}" },
        expect: {
          status: 200,
          bodyContains: {
            id: "${sid}",
            messages: [
              { id: "<id>", ord: 0, role: "user", content: "hello sse", createdAtMs: "<ms>" },
              {
                id: "<id>",
                ord: 1,
                role: "assistant",
                content: "hello sse",
                createdAtMs: "<ms>",
              },
            ],
          },
        },
      },
    ],
  },
  {
    id: "d4.chat.sse-fallback-refuses-an-empty-message",
    package: "D4",
    routes: ["POST /api/v1/sessions", "POST /api/v1/sessions/:id/chat"],
    auth: "paired",
    scenario: "d4-chat",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "empty" } },
        expect: { status: 200, bodyContains: { id: "<id>" } },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/chat",
          body: { content: "" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d4.chat.ws-turn-announces-then-finalizes-then-persists",
    package: "D4",
    routes: ["POST /api/v1/sessions", "POST /api/v1/sessions/:id/messages"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "ws turn" } },
        expect: { status: 200, bodyContains: { id: "<id>" } },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "user", content: "hello ws" },
        },
        expect: { status: 200, bodyContains: { id: "<id>", ord: 0 } },
        capture: { uid: "$.id" },
      },
      {
        // The birth-then-finalize pair is the invariant this case exists for:
        // the client is told an assistant message EXISTS, at a position, before
        // any delta touches it, and the same id comes back `final: true` once it
        // is persisted. A core that only emitted the terminal snapshot would
        // still produce the right transcript on reload and a visibly broken one
        // live, which no HTTP case can see.
        expect: {
          ws: {
            ticket: "valid",
            send: [{ kind: "chat.start", streamId: "ws-turn-1", sessionId: "${sid}" }],
            expect: [
              {
                kind: "chat.message",
                streamId: "ws-turn-1",
                sessionId: "${sid}",
                final: false,
                // Born at the anchoring user message, which is what makes the
                // client place it at the tail rather than guessing.
                afterId: "${uid}",
                // Every field, not just the interesting ones: in differential
                // mode a field no case mentions is compared raw, so a per-run
                // value left out reads as a divergence rather than as silence
                // (D-029). `ord: -1` is the unassigned sentinel a birth carries,
                // and `modelUsed` is the resolved route landing on the message.
                message: {
                  id: "<id>",
                  ord: -1,
                  role: "assistant",
                  content: "",
                  createdAtMs: "<ms>",
                  modelUsed: "default",
                },
              },
              {
                kind: "chat.message",
                streamId: "ws-turn-1",
                sessionId: "${sid}",
                final: true,
                // Already positioned by its birth, so the terminal snapshot
                // carries no position. Re-sending one here would move a bubble
                // the client has already placed.
                afterId: null,
                message: {
                  id: "<id>",
                  ord: 1,
                  role: "assistant",
                  content: "hello ws",
                  createdAtMs: "<ms>",
                  modelUsed: "default",
                },
              },
              { kind: "chat.done", streamId: "ws-turn-1", reason: "stop" },
            ],
          },
          timeoutMs: 10000,
        },
      },
      {
        request: { method: "GET", path: "/api/v1/sessions/${sid}" },
        expect: {
          status: 200,
          bodyContains: {
            messages: [
              { id: "${uid}", ord: 0, role: "user" },
              { id: "<id>", ord: 1, role: "assistant", content: "hello ws" },
            ],
          },
        },
      },
    ],
  },
  {
    id: "d4.chat.ws-two-hop-tool-turn-feeds-the-result-back",
    package: "D4",
    routes: ["POST /api/v1/sessions", "POST /api/v1/sessions/:id/messages"],
    auth: "paired",
    isolate: true,
    // Hop 1 asks for a tool; hop 2 echoes, and what it echoes is the LAST
    // message, which by then is the tool result. So the assistant's final text
    // is the tool outcome, and it can only be that if the core assembled the
    // call, dispatched it, folded the outcome into a `tool` message, and put
    // that message into the transcript it sent back up. Four steps of the
    // dispatch path, asserted by one string.
    llm: {
      kind: "sequence",
      scripts: [
        {
          kind: "tool_calls",
          calls: [{ id: "call_alpha", name: "get_weather", arguments: '{"city":"Jakarta"}' }],
        },
        { kind: "echo" },
      ],
    },
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "two hop" } },
        expect: { status: 200, bodyContains: { id: "<id>" } },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "user", content: "weather?" },
        },
        expect: { status: 200, bodyContains: { id: "<id>", ord: 0 } },
        capture: { uid: "$.id" },
      },
      {
        expect: {
          ws: {
            ticket: "valid",
            send: [{ kind: "chat.start", streamId: "ws-hop-1", sessionId: "${sid}" }],
            // Only the terminal frame is named here. The per-message assertions
            // live in the transcript step below, where a subset match can name
            // four messages by role without also pinning the delta traffic
            // between them. What THIS asserts is that a turn which hops still
            // reaches a natural stop, rather than ending on the hop cap or an
            // error: `reason` is the one field that distinguishes the three.
            expect: [{ kind: "chat.done", streamId: "ws-hop-1", reason: "stop" }],
          },
          timeoutMs: 15000,
        },
      },
      {
        // The durable transcript is where the hop is legible. The tool message
        // carries the arguments the model streamed in two pieces, and the
        // assistant that follows carries the tool's own outcome, which is only
        // reachable through the result being fed back.
        request: { method: "GET", path: "/api/v1/sessions/${sid}" },
        expect: {
          status: 200,
          bodyContains: {
            messages: [
              { id: "${uid}", ord: 0, role: "user", content: "weather?" },
              {
                id: "<id>",
                ord: 1,
                role: "assistant",
                toolCalls: [
                  {
                    // Namespaced `${streamId}:${index}:${modelId}` so two
                    // concurrent turns whose models both say `call_1` cannot
                    // collide in the shared in-flight map. The model still sees
                    // a matched pair, because the tool result carries the same
                    // value.
                    callId: "ws-hop-1:0:call_alpha",
                    toolName: "get_weather",
                    arguments: '{"city":"Jakarta"}',
                  },
                ],
              },
              {
                id: "<id>",
                ord: 2,
                role: "tool",
                callId: "ws-hop-1:0:call_alpha",
                toolName: "get_weather",
                arguments: '{"city":"Jakarta"}',
                status: "failed",
              },
              // The load-bearing assertion of this case. The mock echoes the
              // LAST message, so this text is the tool result verbatim, and it
              // can only be here if the core folded the outcome into a `tool`
              // message AND put that message in the transcript it sent on hop 2.
              // A core that dropped the result, or sent it in the wrong slot,
              // echoes something else rather than failing slowly.
              {
                id: "<id>",
                ord: 3,
                role: "assistant",
                content: '{"error":"tool get_weather not available"}',
              },
            ],
          },
        },
      },
    ],
  },
  {
    id: "d4.chat.ws-a-model-that-only-calls-tools-stops-at-the-hop-limit",
    package: "D4",
    routes: ["POST /api/v1/sessions", "POST /api/v1/sessions/:id/messages"],
    auth: "paired",
    isolate: true,
    // Every hop asks for another tool and never answers. `tools.maxHops`
    // defaults to 5, so the turn stops itself rather than looping forever, and
    // it stops with `hop_limit` rather than an error: the conversation up to
    // that point is valid and fully persisted, which is what the reason code
    // tells the client.
    llm: {
      kind: "sequence",
      scripts: [
        {
          kind: "tool_calls",
          calls: [{ id: "call_again", name: "spin", arguments: "{}" }],
        },
      ],
      loop: true,
    },
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "hop limit" } },
        expect: { status: 200, bodyContains: { id: "<id>" } },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "user", content: "spin forever" },
        },
        expect: { status: 200, bodyContains: { id: "<id>", ord: 0 } },
        capture: { uid: "$.id" },
      },
      {
        expect: {
          ws: {
            ticket: "valid",
            send: [{ kind: "chat.start", streamId: "ws-hop-cap", sessionId: "${sid}" }],
            expect: [{ kind: "chat.done", streamId: "ws-hop-cap", reason: "hop_limit" }],
          },
          timeoutMs: 20000,
        },
      },
    ],
  },
];
