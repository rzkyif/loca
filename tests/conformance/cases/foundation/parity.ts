import type { Case } from "../../runner/types.ts";
import { UNSTABLE_CAPABILITIES } from "./capabilities.ts";

// Parity: what has to survive a core stopping and starting again on the same
// home. Today both boots are the same target, which proves persistence works at
// all. In differential mode the second boot becomes the OTHER core, and the
// same cases become the only end-to-end check that a real existing install
// keeps working after the port. Writing them now, against one target, is what
// makes that swap a one-line change later.
//
// These sit in the foundation slice on purpose: the pin is F6's and the bearer
// is F7's, and both are named byte-exact parity surfaces in shared/interfaces.md.
// Per-package persistence (sessions, settings, memories) belongs to those
// packages' own parity cases, so coverage is attributed where the work is.

export const cases: Case[] = [
  {
    id: "f6.parity.pin-survives-a-restart",
    package: "F6",
    routes: ["GET /api/v1/health"],
    auth: "none",
    isolate: true,
    steps: [
      {
        request: { method: "GET", path: "/api/v1/health" },
        expect: {
          status: 200,
          bodyContains: {
            status: "ok",
            uptimeMs: "<duration>",
            capabilities: UNSTABLE_CAPABILITIES,
          },
        },
      },
      {
        // The cert is regenerated on every boot; the KEY is sealed in the
        // secrets vault, so the pin derived from it must not move. A changed
        // pin here means the key was not persisted or the derivation drifted,
        // and either one silently breaks every paired client.
        handoff: "restart",
        request: { method: "GET", path: "/api/v1/health" },
        expect: {
          status: 200,
          pinUnchanged: true,
          bodyContains: {
            status: "ok",
            uptimeMs: "<duration>",
            capabilities: UNSTABLE_CAPABILITIES,
          },
        },
      },
    ],
  },
  {
    id: "f7.parity.bearer-survives-a-restart",
    package: "F7",
    routes: ["GET /api/v1/pairing/clients"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: { method: "GET", path: "/api/v1/pairing/clients" },
        expect: {
          status: 200,
          // id and the two timestamps are named even though the assertion is
          // about the name: an unmentioned field is compared exactly in
          // differential mode, and these three legitimately differ per core.
          bodyContains: [
            {
              id: "<id>",
              name: "conformance",
              isMe: true,
              createdAtMs: "<ms>",
              lastSeenMs: "<ms>",
            },
          ],
        },
        capture: { clientId: "$.0.id" },
      },
      {
        // Same bearer, same client row, same id. Two parity surfaces ride in
        // this one case, and the name only mentions the first: the bearer
        // authenticating proves the token HASH round-tripped, and the id
        // matching proves the persisted client ROW did. The token hash lives in
        // SQLite under the home, so if either moved, a paired client comes back
        // as a stranger and the user is told to re-pair for no visible reason.
        handoff: "restart",
        request: { method: "GET", path: "/api/v1/pairing/clients" },
        expect: {
          status: 200,
          pinUnchanged: true,
          bodyContains: [
            {
              id: "${clientId}",
              name: "conformance",
              isMe: true,
              createdAtMs: "<ms>",
              lastSeenMs: "<ms>",
            },
          ],
        },
      },
    ],
  },
];
