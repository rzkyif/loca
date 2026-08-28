// D8: the requirements snapshot and the greeting trigger.
//
// # What is asserted, and what deliberately is not
//
// The MODEL rows are asserted exactly, because both cores agree on every field
// of them. The BINARY rows are not asserted at all, and that is a recorded
// difference rather than an oversight: the oracle requires `deno` AND
// `llama-server` (`domain/model.ts:358-362`), while the Rust core requires
// `deno` alone, because it absorbs llama.cpp and sherpa in-process and can
// neither download nor execute the other two. Offering them would put two
// permanently unsatisfiable rows in `missing`, which is the client's app-level
// pending gate, so the app would never leave the pending state.
//
// `bodyContains` with a one-element array is what expresses that: index 0 is
// compared, the tail is not. **`--core=diff` will still report a difference on
// `$.required` and `$.missing` for these cases**, because differential mode
// compares the whole observed body rather than the asserted subset. That
// difference is expected, ruled on at D8's plan gate, and is the same shape
// `cases/d7-binaries.ts` records for `GET /api/v1/binaries`.
//
// # Why the model list is short here
//
// The harness seeds `llm.provider: "external"` (`harness/home.ts:41`), so no
// local LLM weights are required. The embedding model still is: it is only
// dropped when an external provider ALSO has a relevance model configured, and
// the fixture configures none.
//
// # What has no case, and why
//
// `?probe=1` is the consented path that reaches HuggingFace and GitHub for size
// hints. It is exercised by the `only_the_literal_probe_1_turns_on_the_network`
// unit test in both directions; a case here would either send the oracle to a
// real upstream or assert around a mock's sizes, and the negative half (`?probe`
// spelled any other way is NOT a probe) is the half that matters and is
// asserted below.
//
// `POST /api/v1/requirements/download` has no case either: it enqueues real
// multi-gigabyte model downloads by design, and the two cores enqueue different
// binary sets for the reason above.

import type { Case } from "../runner/types.ts";

const EMBED_MODEL = "@second-state/All-MiniLM-L6-v2-Embedding-GGUF/main/all-MiniLM-L6-v2-Q8_0.gguf";

/** The embedding row, exactly, as both cores report it on a fresh home. */
const EMBED_ROW = {
  source: EMBED_MODEL,
  type: "model",
  group: "embed",
  present: false,
  // Absent rather than null on both: nothing has failed to download, and the
  // always-on snapshot does not size anything.
  error: "<absent>",
  sizeHint: "<absent>",
  unavailable: "<absent>",
  version: "<absent>",
};

export const cases: Case[] = [
  {
    id: "d8.requirements.snapshot-needs-a-bearer",
    package: "D8",
    routes: ["GET /api/v1/requirements"],
    auth: "none",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/requirements" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "d8.requirements.reports-the-model-files-the-configuration-needs",
    package: "D8",
    routes: ["GET /api/v1/requirements"],
    auth: "paired",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/requirements" },
        expect: {
          status: 200,
          bodyContains: {
            // Index 0 of each array, compared field for field. The binary rows
            // behind it are the recorded difference; see the header.
            required: [EMBED_ROW],
            // Nothing is on disk in a fresh home, so the same row is missing.
            // Asserting BOTH is the point: a snapshot whose `missing` filter was
            // inverted would still fill `required` correctly.
            missing: [EMBED_ROW],
          },
        },
      },
    ],
  },
  {
    id: "d8.requirements.only-the-literal-probe-1-is-a-probe",
    package: "D8",
    routes: ["GET /api/v1/requirements"],
    auth: "paired",
    steps: [
      {
        // `?probe=true` is NOT a probe. The core compares against the literal
        // "1" (`http/routes/requirements.ts:22`), and this is the switch between
        // a local-only snapshot and one that reaches HuggingFace and GitHub, so
        // a widening here would put an unconsented outbound request on the path
        // a client hits on every connect.
        //
        // Observable as the ABSENCE of a size hint: a probing snapshot fills
        // `sizeHint` for a missing file, and a local-only one cannot.
        request: { method: "GET", path: "/api/v1/requirements?probe=true" },
        expect: { status: 200, bodyContains: { missing: [{ sizeHint: "<absent>" }] } },
      },
      {
        request: { method: "GET", path: "/api/v1/requirements?probe=0" },
        expect: { status: 200, bodyContains: { missing: [{ sizeHint: "<absent>" }] } },
      },
    ],
  },
  {
    id: "d8.greetings.run-needs-a-bearer",
    package: "D8",
    routes: ["POST /api/v1/greetings/run"],
    auth: "none",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/greetings/run", body: {} },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "d8.greetings.run-refuses-an-unknown-or-null-field",
    package: "D8",
    routes: ["POST /api/v1/greetings/run"],
    auth: "paired",
    steps: [
      {
        // The body schema is strict on both sides, and it is now one schema:
        // `validation/greetings.ts`, which the bridge reads into
        // `inbound::GreetingRunBody`, so `.strict()` reaches Rust as
        // `deny_unknown_fields` rather than being hand-written next to it.
        request: {
          method: "POST",
          path: "/api/v1/greetings/run",
          body: { sessionTitel: "a typo the oracle refuses" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // D-098, on a numeric field: `cooldownMs` is `.optional()`, which means
        // `T | undefined` and refuses `null`. It matters more here than most,
        // because a null read as absent would silently restore the DEFAULT
        // dedup window rather than the one the client asked for.
        request: {
          method: "POST",
          path: "/api/v1/greetings/run",
          body: { cooldownMs: null },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    // Its own core: the PATCH below is a settings write every neighbour sharing
    // the boot would inherit.
    isolate: true,
    id: "d8.requirements.a-settings-change-rebroadcasts-the-snapshot",
    package: "D8",
    routes: ["PATCH /api/v1/settings"],
    auth: "paired",
    watch: ["requirements.snapshot"],
    steps: [
      {
        // The one assertion no unit test can make, because a test that built the
        // wiring would be testing its own copy of it: settings is what PUTS a
        // file in `missing`, so a core that recomputes on nothing leaves every
        // client on the snapshot it fetched at connect. The client gates the
        // whole app on that set, which is the "downloads all finished, settings
        // still says Downloading" shape.
        //
        // Any schema key does: both cores recompute on the change edge rather
        // than on which key moved. `llm.contextSize` is the cheapest one that
        // touches no model file.
        request: {
          method: "PATCH",
          path: "/api/v1/settings",
          body: { "llm.contextSize": 8192 },
        },
        expect: {
          status: 200,
          frames: [
            {
              kind: "requirements.snapshot",
              // Held at `<any>`: the rows themselves are asserted exactly by the
              // GET case above, and the binary half differs between the cores
              // for the reason the module header records.
              required: "<any>",
              missing: "<any>",
            },
          ],
        },
      },
    ],
  },
  {
    // Its own core: a greeting mints a session and starts a turn on it, which is
    // global state a neighbour sharing the boot would see in its session list.
    isolate: true,
    id: "d8.greetings.mints-one-session-per-client-window",
    package: "D8",
    routes: ["POST /api/v1/greetings/run", "GET /api/v1/sessions"],
    auth: "paired",
    watch: ["session.created"],
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/greetings/run",
          // No `{datetime}` in the title, so the session can be identified
          // exactly in the listing below. The stamping itself is unit-tested;
          // a timestamp is not assertable on the wire.
          body: {
            sessionTitle: "d8 conformance greeting",
            instruction: "Say hello and nothing else.",
            cooldownMs: 60000,
          },
        },
        expect: {
          status: 200,
          // No `sessionId`: the client navigates on the frame below, not on this
          // response. The Client's own type declares an optional `sessionId`
          // that neither core has ever sent.
          body: { ran: true },
          frames: [
            {
              kind: "session.created",
              // Held at `<any>`: every field of it is generated (id, timestamps)
              // and the session's own shape is D2's contract, not this route's.
              session: "<any>",
              reason: "greeting",
              focus: "show_when_done",
            },
          ],
        },
      },
      {
        // The dedup guard. A crash-looping client must not mint a session per
        // retry, and a suppressed run still answers 200 so an autostarted launch
        // can tell that its window is open.
        request: {
          method: "POST",
          path: "/api/v1/greetings/run",
          body: { sessionTitle: "d8 conformance greeting", cooldownMs: 60000 },
        },
        expect: {
          status: 200,
          body: { ran: false, reason: "recent" },
          // And nothing was minted behind it, which the response alone cannot
          // show: a core that answered `ran: false` and started a session anyway
          // would pass the line above.
          noFrames: ["session.created"],
        },
      },
      {
        // Exactly one session exists, and it is the greeting's. Proves the turn
        // persisted rather than only being announced.
        request: { method: "GET", path: "/api/v1/sessions" },
        expect: {
          status: 200,
          bodyContains: [{ title: "d8 conformance greeting" }],
        },
      },
    ],
  },
];
