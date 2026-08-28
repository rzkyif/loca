import type { Case } from "../../runner/types.ts";

// These upgrade-status assertions are the reason the suite performs the
// WebSocket handshake over a raw TLS socket instead of using the WebSocket API.
// The headless E2E lane documents the unauthorized state as uncoverable there
// precisely because the browser hides the upgrade status; here it is just a
// number to compare.

export const cases: Case[] = [
  {
    id: "f6.ws.ticket-then-upgrade",
    package: "F6",
    routes: ["POST /api/v1/ws/ticket", "GET /ws/v1"],
    auth: "paired",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/ws/ticket" },
        expect: { status: 200, body: { ticket: "<string>" } },
      },
      {
        // A fresh ticket is minted by the runner for the upgrade itself; the
        // step above asserts the mint route's own contract.
        expect: { ws: { ticket: "valid", status: 101 } },
      },
    ],
  },
  {
    id: "f6.ws.a-new-connection-is-seeded-with-the-core-status",
    package: "F6",
    routes: ["GET /ws/v1"],
    auth: "paired",
    watch: ["core.status"],
    steps: [
      {
        // The frame is asserted on the CONNECT, not on the request: the watcher
        // opens its socket before the first step and buffers, so this GET is
        // only something for the step to be. Emission is edge-only on both
        // cores, so a client joining a steady core learns its status from this
        // seed or from nothing at all, and the client's bar reads `core.status`
        // frames and no other source.
        request: { method: "GET", path: "/api/v1/health" },
        expect: {
          status: 200,
          frames: [
            {
              kind: "core.status",
              // The aggregate itself is not contractual between the cores: the
              // Rust core absorbs the sidecars, so its `subsystems` list is a
              // different set. That a snapshot ARRIVES is the assertion.
              snapshot: "<any>",
            },
          ],
        },
      },
    ],
  },
  {
    id: "f6.ws.heartbeat-round-trip",
    package: "F6",
    routes: ["GET /ws/v1"],
    auth: "paired",
    steps: [
      {
        expect: {
          ws: {
            ticket: "valid",
            send: [{ kind: "ping" }],
            expect: [{ kind: "pong" }],
          },
          timeoutMs: 10_000,
        },
      },
    ],
  },
  {
    id: "f6.ws.upgrade-without-a-ticket-is-refused",
    package: "F6",
    routes: ["GET /ws/v1"],
    auth: "none",
    steps: [{ expect: { ws: { ticket: "none", status: 401 } } }],
  },
  {
    id: "f6.ws.upgrade-with-an-invalid-ticket-is-refused",
    package: "F6",
    routes: ["GET /ws/v1"],
    auth: "paired",
    steps: [{ expect: { ws: { ticket: "invalid", status: 401 } } }],
  },
  {
    id: "f6.ws.a-ticket-is-single-use",
    package: "F6",
    routes: ["GET /ws/v1"],
    auth: "paired",
    steps: [{ expect: { ws: { ticket: "spent", status: 401 } } }],
  },
];
