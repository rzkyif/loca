import type { Case } from "../../runner/types.ts";

// The PAKE handshake itself is not expressible as request/response data: it is
// a stateful exchange whose second message depends on the first. It is driven
// by harness/pair.ts (using the SAME CPace implementation the Client uses) and
// asserted by the parity cases and by the mere fact that every `auth: "paired"`
// case below reaches its route. What IS expressible, and what these cases
// cover, is the authorization surface around it.

export const cases: Case[] = [
  {
    id: "f7.pairing.codes-need-authorization",
    package: "F7",
    routes: ["POST /api/v1/pairing/codes"],
    auth: "none",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/pairing/codes", body: {} },
        expect: { status: 401, body: { error: { code: "missing_token", message: "<string>" } } },
      },
    ],
  },
  {
    id: "f7.pairing.codes-with-admin-token",
    package: "F7",
    routes: ["POST /api/v1/pairing/codes"],
    auth: "admin",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/pairing/codes", body: {} },
        expect: {
          status: 200,
          body: { code: "<string>", expiresAtMs: "<ms-future>" },
        },
      },
    ],
  },
  {
    id: "f7.pairing.bad-admin-token-refused",
    package: "F7",
    routes: ["POST /api/v1/pairing/codes"],
    auth: "none",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/pairing/codes",
          headers: { "x-admin-token": "wrong" },
          body: {},
        },
        // A PRESENT but wrong admin token is a different code from a missing
        // one. The case pins the exact code rather than "some 401" because the
        // code is part of the published ERROR_CODES contract: collapsing the
        // three auth codes into one is an observable wire change whether or not
        // anything reads them today, and the distinction is what makes an auth
        // failure diagnosable from a response or a log. No current Client code
        // branches on it (the re-pair path keys off HTTP status, client.ts:379),
        // so do not relax this to a status check on the theory that it has a
        // consumer to satisfy.
        expect: { status: 401, bodyContains: { error: { code: "admin_token_required" } } },
      },
    ],
  },
  {
    id: "f7.pairing.pake-start-rejects-garbage",
    package: "F7",
    routes: ["POST /api/v1/pairing/pake/start"],
    auth: "none",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/pairing/pake/start",
          body: { clientName: "x", sid: "!!not base64!!", msgA: "also not base64" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "f7.pairing.pake-finish-rejects-unknown-id",
    package: "F7",
    routes: ["POST /api/v1/pairing/pake/finish"],
    auth: "none",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/pairing/pake/finish",
          body: { pakeId: "00000000000000000000000000", confirmC: "AAAA" },
        },
        expect: { status: 401, bodyContains: { error: { code: "invalid_pairing_code" } } },
      },
    ],
  },
  {
    id: "f7.pairing.lists-the-paired-clients",
    package: "F7",
    routes: ["GET /api/v1/pairing/clients"],
    auth: "paired",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/pairing/clients" },
        expect: {
          status: 200,
          // A bare array, not an envelope. A subset match because the
          // scenario's second identity may or may not exist depending on which
          // cases share this boot, which is not this case's business.
          bodyContains: [
            {
              id: "<id>",
              name: "conformance",
              createdAtMs: "<ms>",
              lastSeenMs: "<ms>",
              isMe: true,
            },
          ],
        },
      },
    ],
  },
  {
    id: "f7.pairing.unauthenticated-routes-are-refused",
    package: "F7",
    routes: ["GET /api/v1/pairing/clients", "POST /api/v1/ws/ticket"],
    auth: "none",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/pairing/clients" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/ws/ticket" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "f7.pairing.rotate-invalidates-the-old-token",
    package: "F7",
    routes: ["POST /api/v1/pairing/rotate"],
    auth: "paired",
    // Rotation invalidates the scenario's cached bearer, so this case cannot
    // share a boot with anything that runs after it.
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/pairing/rotate" },
        expect: { status: 200, body: { token: "<string>" } },
      },
      {
        // The runner still holds the pre-rotation bearer. A PRESENT but no
        // longer valid token is invalid_token, distinct from missing_token.
        // Both are 401, so a status-only assertion would pass even if the port
        // collapsed them; the code is the part that carries the information.
        request: { method: "GET", path: "/api/v1/pairing/clients" },
        expect: { status: 401, bodyContains: { error: { code: "invalid_token" } } },
      },
    ],
  },
];
