// D2: sessions, messages and attachments.
//
// Written against the contract in `@loca/shared` and the route handlers, not
// against observed output. Two of the twelve routes are not covered here and
// both are deliberate:
//
// - `POST /:id/attachments` and `GET /:id/attachments/:attId` need a multipart
//   request body, which `RequestSpec` cannot express yet. They are covered by
//   route-level Rust tests over the assembled router until the runner grows a
//   form variant.
// - `POST /:id/chat` is SSE and needs a language model behind it, so its cases
//   land with D4 rather than here. The handler itself is covered by Rust tests
//   against a scripted chat engine.

import type { Case } from "../runner/types.ts";

/** Everything `POST /api/v1/sessions` answers with except the title, which is
 *  the caller's. Spread it and add the title: `{ ...CREATED, title: "x" }`.
 *
 *  Every field here is per-run (a ULID, the pairing's client id, a clock
 *  reading), so a step that only wants the id still has to assert the shape.
 *  In differential mode a field NO case mentions is compared raw between the
 *  two cores, so leaving one out reads as a divergence rather than as silence
 *  (D-029). Asserting it with a placeholder is also the stronger case: the
 *  alternative was not checking the field at all. */
const CREATED = {
  id: "<id>",
  ownerClientId: "<id>",
  createdAtMs: "<ms>",
  updatedAtMs: "<ms>",
  // Absent, not null: see `create-patch-get-delete` below.
  tokenUsage: "<absent>",
  temporary: "<absent>",
};

export const cases: Case[] = [
  {
    id: "d2.sessions.create-patch-get-delete",
    package: "D2",
    routes: [
      "POST /api/v1/sessions",
      "PATCH /api/v1/sessions/:id",
      "GET /api/v1/sessions/:id",
      "DELETE /api/v1/sessions/:id",
    ],
    auth: "paired",
    scenario: "d2",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "first" } },
        expect: {
          status: 200,
          body: {
            id: "<id>",
            ownerClientId: "<id>",
            title: "first",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
            // Absent, not null: `tokenUsage` and `temporary` are only written
            // once they have a value, and a client that reads `temporary` as
            // present-and-false would treat every session as a scratch one.
            tokenUsage: "<absent>",
            temporary: "<absent>",
          },
        },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/sessions/${sid}",
          body: { title: "second" },
        },
        // A narrow response by contract (`Pick<Session, "id" | "title">`), so
        // this is an exact match rather than a subset: a handler returning the
        // whole session would be a wire change.
        expect: { status: 200, body: { id: "${sid}", title: "second" } },
      },
      {
        request: { method: "GET", path: "/api/v1/sessions/${sid}" },
        expect: {
          status: 200,
          body: {
            id: "${sid}",
            ownerClientId: "<id>",
            title: "second",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
            tokenUsage: "<absent>",
            temporary: "<absent>",
            messages: [],
          },
        },
      },
      {
        request: { method: "DELETE", path: "/api/v1/sessions/${sid}" },
        expect: { status: 204 },
      },
      {
        request: { method: "GET", path: "/api/v1/sessions/${sid}" },
        // `session_not_found`, not the generic `not_found`: both are 404, and
        // the distinction is part of the published `ERROR_CODES` contract.
        expect: { status: 404, bodyContains: { error: { code: "session_not_found" } } },
      },
    ],
  },
  {
    id: "d2.sessions.list-is-a-bare-array-newest-first",
    package: "D2",
    routes: ["GET /api/v1/sessions", "POST /api/v1/sessions", "PATCH /api/v1/sessions/:id"],
    auth: "paired",
    // Reads every session this client owns, so it cannot share a boot with
    // cases that create their own.
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "older" } },
        expect: { status: 200, body: { ...CREATED, title: "older" } },
        capture: { older: "$.id" },
      },
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "newer" } },
        expect: { status: 200, body: { ...CREATED, title: "newer" } },
        capture: { newer: "$.id" },
      },
      {
        // Touching the older one makes the ordering assertion about
        // `updatedAtMs` rather than about creation order.
        request: {
          method: "PATCH",
          path: "/api/v1/sessions/${older}",
          body: { title: "older, touched" },
        },
        expect: { status: 200, body: { id: "${older}", title: "older, touched" } },
      },
      {
        request: { method: "GET", path: "/api/v1/sessions" },
        // A BARE ARRAY, with no `{ items: [...] }` envelope, and each entry is
        // the summary shape rather than the whole session: no `ownerClientId`,
        // no `messages`. Order is contractual (newest first), so this is not
        // an `unordered` comparison.
        expect: {
          status: 200,
          body: [
            {
              id: "${older}",
              title: "older, touched",
              createdAtMs: "<ms>",
              updatedAtMs: "<ms>",
              messageCount: 0,
              summary: [],
            },
            {
              id: "${newer}",
              title: "newer",
              createdAtMs: "<ms>",
              updatedAtMs: "<ms>",
              messageCount: 0,
              summary: [],
            },
          ],
        },
      },
    ],
  },
  {
    id: "d2.sessions.create-defaults-to-an-empty-title",
    package: "D2",
    routes: ["POST /api/v1/sessions"],
    auth: "paired",
    scenario: "d2",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: {} },
        expect: { status: 200, body: { ...CREATED, title: "" } },
      },
    ],
  },
  {
    id: "d2.sessions.rejects-a-body-that-does-not-match-the-contract",
    package: "D2",
    routes: ["POST /api/v1/sessions", "PATCH /api/v1/sessions/:id"],
    auth: "paired",
    scenario: "d2",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: {} },
        expect: { status: 200, body: { ...CREATED, title: "" } },
        capture: { sid: "$.id" },
      },
      // NOT asserted here: `{ title: 42 }` and `{ temporary: "yes" }`. The two
      // cores disagree, on purpose. The Deno core reads this body with a bare
      // cast and persists a number as a title (200); the Rust core deserializes
      // `CreateSessionRequest` and rejects it (400, `validation_error`), which
      // is what the contract says. A case asserting either answer would be red
      // on one core for the whole of Stage 2, so the tightening is covered by
      // the Rust route tests until cutover retires the oracle. See D-026.
      {
        // PATCH requires the title the type marks optional: a PATCH with
        // nothing to change is malformed, not a no-op.
        request: { method: "PATCH", path: "/api/v1/sessions/${sid}", body: {} },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d2.sessions.a-temporary-session-is-reachable-but-never-listed",
    package: "D2",
    routes: ["POST /api/v1/sessions", "GET /api/v1/sessions/:id", "GET /api/v1/sessions"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { temporary: true } },
        expect: { status: 200, body: { ...CREATED, title: "", temporary: true } },
        capture: { sid: "$.id" },
      },
      {
        request: { method: "GET", path: "/api/v1/sessions/${sid}" },
        expect: {
          status: 200,
          body: { ...CREATED, id: "${sid}", title: "", temporary: true, messages: [] },
        },
      },
      {
        // Never listed, which is what makes it unreachable once the client
        // navigates away.
        request: { method: "GET", path: "/api/v1/sessions" },
        expect: { status: 200, body: [] },
      },
    ],
  },
  {
    id: "d2.sessions.one-client-cannot-reach-another-clients-session",
    package: "D2",
    routes: [
      "POST /api/v1/sessions",
      "GET /api/v1/sessions/:id",
      "PATCH /api/v1/sessions/:id",
      "DELETE /api/v1/sessions/:id",
      "GET /api/v1/sessions",
    ],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "mine" } },
        expect: { status: 200, body: { ...CREATED, title: "mine" } },
        capture: { sid: "$.id" },
      },
      {
        request: { method: "GET", path: "/api/v1/sessions/${sid}", auth: "paired:other" },
        // `session_not_found` rather than `forbidden`, so a wrong owner and a
        // wrong id are indistinguishable and cross-client probing learns
        // nothing.
        expect: { status: 404, bodyContains: { error: { code: "session_not_found" } } },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/sessions/${sid}",
          body: { title: "hijacked" },
          auth: "paired:other",
        },
        expect: { status: 404, bodyContains: { error: { code: "session_not_found" } } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/sessions/${sid}", auth: "paired:other" },
        expect: { status: 404, bodyContains: { error: { code: "session_not_found" } } },
      },
      {
        request: { method: "GET", path: "/api/v1/sessions", auth: "paired:other" },
        expect: { status: 200, body: [] },
      },
      {
        // Untouched by any of it.
        request: { method: "GET", path: "/api/v1/sessions/${sid}" },
        expect: {
          status: 200,
          body: { ...CREATED, id: "${sid}", title: "mine", messages: [] },
        },
      },
    ],
  },
  {
    id: "d2.sessions.requires-a-bearer",
    package: "D2",
    routes: ["GET /api/v1/sessions", "POST /api/v1/sessions"],
    auth: "none",
    scenario: "d2",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/sessions" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
      {
        request: {
          method: "GET",
          path: "/api/v1/sessions",
          headers: { authorization: "Bearer not-a-real-token" },
        },
        // A distinct code from `missing_token`: no credential and a rejected
        // credential are different states, and both are 401.
        expect: { status: 401, bodyContains: { error: { code: "invalid_token" } } },
      },
    ],
  },
  {
    id: "d2.messages.append-patch-delete",
    package: "D2",
    routes: [
      "POST /api/v1/sessions/:id/messages",
      "PATCH /api/v1/sessions/:id/messages/:msgId",
      "DELETE /api/v1/sessions/:id/messages/:msgId",
      "GET /api/v1/sessions/:id",
    ],
    auth: "paired",
    scenario: "d2",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "with messages" } },
        expect: { status: 200, body: { ...CREATED, title: "with messages" } },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "user", content: "hello" },
        },
        // The server fills in the id and the ord the client omitted.
        expect: { status: 200, body: { id: "<id>", ord: 0 } },
        capture: { mid: "$.id" },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "assistant", content: "hi back" },
        },
        expect: { status: 200, body: { id: "<id>", ord: 1 } },
        capture: { mid2: "$.id" },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/sessions/${sid}/messages/${mid}",
          body: { content: "edited" },
        },
        // The ord is pinned: a content patch never reorders.
        expect: { status: 200, body: { id: "${mid}", ord: 0 } },
      },
      {
        request: { method: "GET", path: "/api/v1/sessions/${sid}" },
        expect: {
          status: 200,
          bodyContains: {
            ...CREATED,
            id: "${sid}",
            messages: [
              { id: "${mid}", role: "user", content: "edited", ord: 0, createdAtMs: "<ms>" },
              { id: "${mid2}", role: "assistant", content: "hi back", ord: 1, createdAtMs: "<ms>" },
            ],
          },
        },
      },
      {
        request: { method: "DELETE", path: "/api/v1/sessions/${sid}/messages/${mid}" },
        expect: { status: 204 },
      },
      {
        request: { method: "GET", path: "/api/v1/sessions/${sid}" },
        expect: {
          status: 200,
          bodyContains: {
            ...CREATED,
            id: "${sid}",
            messages: [{ id: "${mid2}", role: "assistant", ord: 1, createdAtMs: "<ms>" }],
          },
        },
      },
    ],
  },
  {
    id: "d2.messages.the-summary-is-computed-server-side",
    package: "D2",
    routes: ["POST /api/v1/sessions/:id/messages", "GET /api/v1/sessions"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "summarized" } },
        expect: { status: 200, body: { ...CREATED, title: "summarized" } },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "system", content: "you are helpful" },
        },
        expect: { status: 200, body: { id: "<id>", ord: 0 } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "user", content: "  what   is\nup? " },
        },
        expect: { status: 200, body: { id: "<id>", ord: 1 } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "assistant", content: "not much" },
        },
        expect: { status: 200, body: { id: "<id>", ord: 2 } },
      },
      {
        request: { method: "GET", path: "/api/v1/sessions" },
        // Only user and assistant contribute, whitespace is collapsed, and the
        // assistant is labelled "agent" because the client supplies the real
        // configured name.
        expect: {
          status: 200,
          body: [
            {
              id: "${sid}",
              title: "summarized",
              createdAtMs: "<ms>",
              updatedAtMs: "<ms>",
              messageCount: 3,
              summary: [
                { role: "user", text: "what is up?" },
                { role: "agent", text: "not much" },
              ],
            },
          ],
        },
      },
    ],
  },
  {
    id: "d2.messages.rejects-a-body-the-role-does-not-allow",
    package: "D2",
    routes: ["POST /api/v1/sessions/:id/messages", "PATCH /api/v1/sessions/:id/messages/:msgId"],
    auth: "paired",
    scenario: "d2",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: {} },
        expect: { status: 200, body: { ...CREATED, title: "" } },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "user", content: "hello" },
        },
        expect: { status: 200, body: { id: "<id>", ord: 0 } },
        capture: { mid: "$.id" },
      },
      {
        // No role at all: the union is discriminated on it.
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { content: "hello" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "wizard", content: "hello" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // An assistant-only field on a user message. This is the guard the
        // per-role schemas exist for, and the reason the patch route looks up
        // the row before validating.
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "user", content: "hello", toolCalls: [] },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/sessions/${sid}/messages/${mid}",
          body: { toolCalls: [] },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // A patch cannot re-role a message.
        request: {
          method: "PATCH",
          path: "/api/v1/sessions/${sid}/messages/${mid}",
          body: { role: "assistant" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d2.messages.a-missing-message-is-message-not-found",
    package: "D2",
    routes: [
      "PATCH /api/v1/sessions/:id/messages/:msgId",
      "DELETE /api/v1/sessions/:id/messages/:msgId",
    ],
    auth: "paired",
    scenario: "d2",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: {} },
        expect: { status: 200, body: { ...CREATED, title: "" } },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/sessions/${sid}/messages/01NOSUCHMESSAGE0000000000",
          body: { content: "x" },
        },
        // `message_not_found`, not `session_not_found` and not the generic
        // `not_found`: the session was found and the message was not.
        expect: { status: 404, bodyContains: { error: { code: "message_not_found" } } },
      },
      {
        request: {
          method: "DELETE",
          path: "/api/v1/sessions/${sid}/messages/01NOSUCHMESSAGE0000000000",
        },
        expect: { status: 404, bodyContains: { error: { code: "message_not_found" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/01NOSUCHSESSION0000000000/messages",
          body: { role: "user", content: "x" },
        },
        expect: { status: 404, bodyContains: { error: { code: "session_not_found" } } },
      },
    ],
  },
  {
    id: "d2.sessions.regenerate-title-returns-204",
    package: "D2",
    routes: ["POST /api/v1/sessions/:id/regenerate-title"],
    auth: "paired",
    scenario: "d2",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "before" } },
        expect: { status: 200, body: { ...CREATED, title: "before" } },
        capture: { sid: "$.id" },
      },
      {
        // Fire and forget: generation streams in the background and the client
        // learns the result from `session.updated` frames, so the route itself
        // only acknowledges. The generated title is D4's to assert.
        request: { method: "POST", path: "/api/v1/sessions/${sid}/regenerate-title" },
        expect: { status: 204 },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/01NOSUCHSESSION0000000000/regenerate-title",
        },
        expect: { status: 404, bodyContains: { error: { code: "session_not_found" } } },
      },
    ],
  },
  {
    id: "d2.sessions.survive-a-restart",
    package: "D2",
    routes: ["POST /api/v1/sessions", "POST /api/v1/sessions/:id/messages", "GET /api/v1/sessions"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "persisted" } },
        expect: { status: 200, body: { ...CREATED, title: "persisted" } },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "user", content: "still here?" },
        },
        expect: { status: 200, body: { id: "<id>", ord: 0 } },
        capture: { mid: "$.id" },
      },
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { temporary: true } },
        expect: { status: 200, body: { ...CREATED, title: "", temporary: true } },
        capture: { tid: "$.id" },
      },
      {
        // Sessions are JSON files under `<home>/sessions/`, which is part of the
        // on-disk parity surface. Everything below has to survive a reboot on
        // the same home, and in differential mode this is the end-to-end check
        // that an existing install still reads after the port.
        handoff: "restart",
        request: { method: "GET", path: "/api/v1/sessions/${sid}" },
        expect: {
          status: 200,
          pinUnchanged: true,
          bodyContains: {
            ...CREATED,
            id: "${sid}",
            title: "persisted",
            messages: [
              {
                id: "${mid}",
                role: "user",
                content: "still here?",
                ord: 0,
                createdAtMs: "<ms>",
              },
            ],
          },
        },
      },
      {
        request: { method: "GET", path: "/api/v1/sessions" },
        expect: {
          status: 200,
          body: [
            {
              id: "${sid}",
              title: "persisted",
              messageCount: 1,
              createdAtMs: "<ms>",
              updatedAtMs: "<ms>",
              summary: [{ role: "user", text: "still here?" }],
            },
          ],
        },
      },
      {
        // A temporary session lives only in core memory, so a restart is the
        // one thing that always discards it.
        request: { method: "GET", path: "/api/v1/sessions/${tid}" },
        expect: { status: 404, bodyContains: { error: { code: "session_not_found" } } },
      },
    ],
  },
];
