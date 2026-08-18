// D10: scheduled prompts.
//
// Five routes: the list, the create, the patch, the delete, and the manual run.
//
// What these deliberately do NOT assert, and why:
//
//   - No case runs an ENABLED schedule through `POST /{id}/run`. That opens a
//     real automated session and starts a chat turn, so the assertion would be
//     about the chat pipeline rather than about these five routes, and its
//     timing would come from the mock LLM. `run` is covered here by its two
//     refusals; the firing path is covered by unit tests next to the code,
//     where a turn can be scripted directly.
//   - No case asserts a schedule FIRING on its timer. The soonest occurrence a
//     route can create is minutes away, and reaching it would mean either
//     sleeping or writing `next_run_at_ms` behind the API. Both are the
//     scheduler's internals rather than its contract.
//   - `nextRunAtMs` is asserted as `<ms-future>` rather than as a literal. It
//     is derived from the core's LOCAL wall clock, so its value depends on the
//     machine's timezone and on when in the week the suite runs. What is
//     contractual is that it is epoch MILLISECONDS in the future, which is
//     exactly what that matcher pins, and it is the assertion that catches the
//     seconds-instead-of-ms mistake a port makes.

import type { Case } from "../runner/types.ts";

/** The body the client's schedule editor sends. A weekly is used throughout
 *  because it is the only kind whose occurrence is always in the future without
 *  the case having to compute one. */
const WEEKLY_DRAFT = {
  title: "Weekly review",
  instruction: "Summarise what I worked on this week.",
  schedule: { kind: "weekly", weekdays: [1], hour: 9, minute: 0 },
  runMissed: false,
};

/** A freshly created schedule, as both cores must report it. Exact, including
 *  the key set: `lastRunAtMs` is ABSENT rather than null, which is how the
 *  client tells a schedule that has never run from one that has. */
const FRESH = {
  id: "<id>",
  ownerClientId: "<id>",
  title: "Weekly review",
  instruction: "Summarise what I worked on this week.",
  schedule: { kind: "weekly", weekdays: [1], hour: 9, minute: 0 },
  runMissed: false,
  enabled: true,
  lastRunAtMs: "<absent>",
  nextRunAtMs: "<ms-future>",
  createdAtMs: "<ms>",
  updatedAtMs: "<ms>",
};

export const cases: Case[] = [
  {
    id: "d10.scheduled-prompts.round-trip",
    package: "D10",
    routes: [
      "GET /api/v1/scheduled-prompts",
      "POST /api/v1/scheduled-prompts",
      "PATCH /api/v1/scheduled-prompts/{id}",
      "DELETE /api/v1/scheduled-prompts/{id}",
    ],
    auth: "paired",
    scenario: "d10-crud",
    isolate: true,
    steps: [
      {
        request: { method: "GET", path: "/api/v1/scheduled-prompts" },
        expect: { status: 200, body: { scheduledPrompts: [] } },
      },
      {
        request: { method: "POST", path: "/api/v1/scheduled-prompts", body: WEEKLY_DRAFT },
        expect: { status: 201, body: FRESH },
        capture: { sid: "$.id" },
      },
      {
        request: { method: "GET", path: "/api/v1/scheduled-prompts" },
        expect: { status: 200, body: { scheduledPrompts: [FRESH] } },
      },
      {
        // A rename must not push the schedule forward. The armed occurrence is
        // still in the future either way, so this asserts the shape rather than
        // the instant; the "only recompute when something that defines it
        // changed" matrix is a unit test, where the two instants can be
        // compared.
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { title: "Renamed" },
        },
        expect: {
          status: 200,
          body: { ...FRESH, title: "Renamed" },
        },
      },
      {
        // Disabling parks the row: `nextRunAtMs` goes ABSENT, not null. A core
        // that sent null would still render, and the client would show a
        // disabled schedule as armed.
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { enabled: false },
        },
        expect: {
          status: 200,
          body: { ...FRESH, title: "Renamed", enabled: false, nextRunAtMs: "<absent>" },
        },
      },
      {
        // Re-enabling re-derives it.
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { enabled: true },
        },
        expect: { status: 200, body: { ...FRESH, title: "Renamed" } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/scheduled-prompts/${sid}" },
        expect: { status: 200, body: { ok: true } },
      },
      {
        request: { method: "GET", path: "/api/v1/scheduled-prompts" },
        expect: { status: 200, body: { scheduledPrompts: [] } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/scheduled-prompts/${sid}" },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
    ],
  },
  {
    // D-083. A weekly with no weekdays matches no day, so before this guard the
    // Rust core accepted it, stored it, reported a `nextRunAtMs`, and then
    // never fired it. The oracle refuses it in Zod; the port refuses it in the
    // store, which is the only point on all three write paths.
    //
    // This is the case that proves the two cores now agree, and it is the
    // reason the ADR was filed against this package rather than fixed in
    // passing.
    id: "d10.scheduled-prompts.refuses-a-weekly-with-no-weekdays",
    package: "D10",
    routes: ["POST /api/v1/scheduled-prompts", "PATCH /api/v1/scheduled-prompts/{id}"],
    auth: "paired",
    scenario: "d10-validation",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/scheduled-prompts",
          body: {
            ...WEEKLY_DRAFT,
            schedule: { kind: "weekly", weekdays: [], hour: 9, minute: 0 },
          },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // And nothing was stored: a refusal that still writes the row is the
        // same defect wearing an error message.
        request: { method: "GET", path: "/api/v1/scheduled-prompts" },
        expect: { status: 200, body: { scheduledPrompts: [] } },
      },
      {
        request: { method: "POST", path: "/api/v1/scheduled-prompts", body: WEEKLY_DRAFT },
        expect: { status: 201 },
        capture: { sid: "$.id" },
      },
      {
        // The same guard on the patch path. A guard on create alone leaves the
        // schedule editable into the broken state.
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { schedule: { kind: "weekly", weekdays: [], hour: 9, minute: 0 } },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // The original schedule survived the refused patch.
        request: { method: "GET", path: "/api/v1/scheduled-prompts" },
        expect: {
          status: 200,
          bodyContains: {
            scheduledPrompts: [{ schedule: { kind: "weekly", weekdays: [1], hour: 9, minute: 0 } }],
          },
        },
      },
    ],
  },
  {
    // The patch body's own constraints, none of which the create body's cover:
    // it is a separate Zod schema (`validation/scheduled-prompt.ts`), strict,
    // with `.min(1)` on the two strings and every field optional.
    //
    // The `null` steps are the D-098 pair, and they are the reason absence is
    // asserted in the same case: Zod's `.optional()` means `T | undefined` and
    // REFUSES `null`, while serde's `Option<T>` accepted it and mapped it to
    // `None`, so a null field read as "not supplied" and the patch silently
    // succeeded. The fix is a generator pass, so the two halves have to be
    // pinned together: reject an explicit null, and keep accepting the absence
    // that null was being confused with.
    id: "d10.scheduled-prompts.patch-body-refuses-blank-unknown-and-null-fields",
    package: "D10",
    routes: ["PATCH /api/v1/scheduled-prompts/{id}"],
    auth: "paired",
    scenario: "d10-validation",
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/scheduled-prompts", body: WEEKLY_DRAFT },
        expect: { status: 201, body: FRESH },
        capture: { sid: "$.id" },
      },
      {
        // `.min(1)`, which survives the bridge as a newtype whose `Deserialize`
        // parses through `FromStr`.
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { title: "" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // `.strict()`, which survives as `deny_unknown_fields`.
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { titel: "a typo the oracle refuses" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // D-098. An explicit null on an optional field, which the oracle refuses
        // and the port used to read as "field not supplied".
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { title: null },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // D-111. The patch body used to bound neither `title` nor
        // `instruction`, where create caps them at 200 and 10 000, so a client
        // could create a schedule inside the limits and immediately widen it
        // past them. Both bodies now share one pair of field schemas.
        //
        // `maxLength` on a string DOES survive the bridge (as a newtype whose
        // `Deserialize` counts chars), unlike a numeric `maximum` or an array
        // `maxItems`, which do not. See D-095.
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { title: "x".repeat(201) },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // And the cap is a CAP, not a ban: exactly 200 is accepted. Without
        // this, an off-by-one that refused everything would pass the step above.
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { title: "x".repeat(200) },
        },
        // A full body with placeholders for the unstable fields, not a bare
        // status: diff mode compares WHOLE bodies, so `status: 200` alone leaves
        // the ids and timestamps to differ and the case reports a false
        // disagreement. The runner says exactly this when it happens.
        expect: {
          status: 200,
          body: { ...FRESH, title: "x".repeat(200) },
        },
      },
      {
        // The same for a non-string field, so the fix is not mistaken for the
        // `.min(1)` newtype doing the work.
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { enabled: null },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // The other half of D-098, and the half a naive fix breaks: ABSENCE is
        // still valid. Every field here is optional, so a body naming one of
        // them and omitting the other four has to be accepted.
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { title: "Renamed" },
        },
        expect: { status: 200, body: { ...FRESH, title: "Renamed" } },
      },
      {
        // And none of the four refusals landed a partial write.
        request: { method: "GET", path: "/api/v1/scheduled-prompts" },
        expect: {
          status: 200,
          body: { scheduledPrompts: [{ ...FRESH, title: "Renamed" }] },
        },
      },
    ],
  },
  {
    id: "d10.scheduled-prompts.refuses-a-schedule-that-can-never-fire",
    package: "D10",
    routes: ["POST /api/v1/scheduled-prompts"],
    auth: "paired",
    scenario: "d10-validation",
    steps: [
      {
        // A `once` whose instant has passed. Storing it would report success
        // for a schedule with no occurrence at all.
        request: {
          method: "POST",
          path: "/api/v1/scheduled-prompts",
          body: { ...WEEKLY_DRAFT, schedule: { kind: "once", atMs: 1 } },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // An hour of 24. Zod caps it at 23; typify carries no `maximum`, so on
        // the Rust side this is enforced by hand or not at all (D-095).
        request: {
          method: "POST",
          path: "/api/v1/scheduled-prompts",
          body: {
            ...WEEKLY_DRAFT,
            schedule: { kind: "weekly", weekdays: [1], hour: 24, minute: 0 },
          },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // A weekday of 7. Same mechanism.
        request: {
          method: "POST",
          path: "/api/v1/scheduled-prompts",
          body: {
            ...WEEKLY_DRAFT,
            schedule: { kind: "weekly", weekdays: [7], hour: 9, minute: 0 },
          },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // An interval of zero. Carried as `NonZeroU64`, so this one the bridge
        // DID keep; asserted so a regeneration that loses it is caught here
        // rather than as a core spinning on a schedule that is due forever.
        request: {
          method: "POST",
          path: "/api/v1/scheduled-prompts",
          body: { ...WEEKLY_DRAFT, schedule: { kind: "interval", everyMinutes: 0 } },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // An unknown schedule kind.
        request: {
          method: "POST",
          path: "/api/v1/scheduled-prompts",
          body: { ...WEEKLY_DRAFT, schedule: { kind: "fortnightly", hour: 9, minute: 0 } },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // An empty title, and an absent one.
        request: {
          method: "POST",
          path: "/api/v1/scheduled-prompts",
          body: { ...WEEKLY_DRAFT, title: "" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/scheduled-prompts",
          body: { instruction: "x", schedule: WEEKLY_DRAFT.schedule, runMissed: false },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // The patch body is `.strict()`, and it is a route-local Zod schema, so
        // per D-093 the bridge never saw it and the Rust core enforces this by
        // hand or not at all.
        request: {
          method: "POST",
          path: "/api/v1/scheduled-prompts",
          body: WEEKLY_DRAFT,
        },
        expect: { status: 201 },
        capture: { vid: "$.id" },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${vid}",
          body: { unexpected: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${vid}",
          body: { title: "" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d10.scheduled-prompts.run-refuses-a-disabled-schedule",
    package: "D10",
    routes: ["POST /api/v1/scheduled-prompts/{id}/run"],
    auth: "paired",
    scenario: "d10-run",
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/scheduled-prompts", body: WEEKLY_DRAFT },
        expect: { status: 201 },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { enabled: false },
        },
        expect: { status: 200 },
      },
      {
        request: { method: "POST", path: "/api/v1/scheduled-prompts/${sid}/run", body: {} },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/scheduled-prompts/unknown-id/run", body: {} },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
    ],
  },
  {
    // Owner scoping. A schedule opens a session on its owner's behalf and runs
    // an instruction that owner wrote, so a second paired client reaching one
    // is not a listing bug: it is a client making another client's core talk to
    // a model on its behalf.
    id: "d10.scheduled-prompts.are-private-to-their-owner",
    package: "D10",
    routes: [
      "GET /api/v1/scheduled-prompts",
      "PATCH /api/v1/scheduled-prompts/{id}",
      "DELETE /api/v1/scheduled-prompts/{id}",
      "POST /api/v1/scheduled-prompts/{id}/run",
    ],
    auth: "paired",
    scenario: "d10-isolation",
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/scheduled-prompts", body: WEEKLY_DRAFT },
        expect: { status: 201 },
        capture: { sid: "$.id" },
      },
      {
        request: {
          method: "GET",
          path: "/api/v1/scheduled-prompts",
          auth: "paired:other",
        },
        expect: { status: 200, body: { scheduledPrompts: [] } },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/${sid}",
          body: { title: "Theirs now" },
          auth: "paired:other",
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/scheduled-prompts/${sid}/run",
          body: {},
          auth: "paired:other",
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: {
          method: "DELETE",
          path: "/api/v1/scheduled-prompts/${sid}",
          auth: "paired:other",
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        // The refused verbs did not in fact write, and did not delete.
        request: { method: "GET", path: "/api/v1/scheduled-prompts" },
        expect: { status: 200, body: { scheduledPrompts: [FRESH] } },
      },
    ],
  },
  {
    id: "d10.scheduled-prompts.require-a-paired-client",
    package: "D10",
    routes: [
      "GET /api/v1/scheduled-prompts",
      "POST /api/v1/scheduled-prompts",
      "PATCH /api/v1/scheduled-prompts/{id}",
      "DELETE /api/v1/scheduled-prompts/{id}",
      "POST /api/v1/scheduled-prompts/{id}/run",
    ],
    auth: "none",
    scenario: "d10-auth",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/scheduled-prompts" },
        expect: { status: 401 },
      },
      {
        request: { method: "POST", path: "/api/v1/scheduled-prompts", body: WEEKLY_DRAFT },
        expect: { status: 401 },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/scheduled-prompts/any",
          body: { title: "x" },
        },
        expect: { status: 401 },
      },
      {
        request: { method: "DELETE", path: "/api/v1/scheduled-prompts/any" },
        expect: { status: 401 },
      },
      {
        request: { method: "POST", path: "/api/v1/scheduled-prompts/any/run", body: {} },
        expect: { status: 401 },
      },
    ],
  },
];
