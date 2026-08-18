// Cutover: an install written by the Deno core, opened by the Rust core.
//
// The port is a big-bang swap with no dual-shipping period (D-000). One day the
// Deno core stops shipping and the Rust core boots on the tree it left behind,
// so the single property the whole migration rests on is:
//
//     state the Deno core wrote is state the Rust core can read.
//
// Nothing else in the port tests that. A package's own Rust tests are written
// from the same reading of the TypeScript as the Rust itself; the restart parity
// cases next door prove each core survives its OWN reboot, which is a different
// claim; and F4's migration fixture proves the schema half from the inside. This
// file is the observable half, across implementations, from the wire.
//
// Every assertion below is ALSO satisfied by the first core simply restarting,
// and that is not a flaw in the assertions: "the pin has not moved, the bearer
// still works, the session is still there" is what a successful hand-off looks
// like, and it is also what a hand-off that never happened looks like. The thing
// that separates them is which binary is answering, which the harness verifies
// against the OS on every swap (harness/core.ts, `assertServedBy`). Read that
// before adding a case here, or a green result will mean less than it appears to.
//
// The direction is fixed and not symmetric. A home seeded for the Rust core
// carries one helper binary and the Deno core refuses to boot without four, so
// deno -> rust is the only bootable direction; it is also the only direction the
// cutover claim is about. These run once in every mode, `--core=deno` included,
// and need both binaries present.
//
// The pin is F6's and the bearer is F7's, both named byte-exact parity surfaces
// in shared/interfaces.md. The session case is packaged D2 because the routes it
// covers are D2's, even though the swap machinery is F8's: a failure there is a
// defect in D2's persistence, and the report should say so.
//
// COVERAGE, and how to decide what belongs here. One case per PERSISTENCE
// MECHANISM, not per package, because that is the axis a cutover fails along:
//
//   sealed vault           the pin (tls-key) and the secrets listing
//   SQLite                 the pairing rows, the client_settings overlay
//   JSON files             settings.json, the sessions tree
//   markdown + an index    memories, where the files and the rows can disagree
//   a DERIVED value        nextRunAtMs, recomputed by whichever core booted
//
// That last row is the one most easily forgotten and the worst when it breaks,
// because a derived value is not read from disk at all: it is recomputed, so a
// port can persist the input perfectly and still produce a different answer. A
// schedule that quietly never fires again is indistinguishable from a schedule
// that has not come due.
//
// The last two are COVERED as of Stage 5, and both needed `plant` (see
// runner/types.ts) because neither fixture can be produced by an HTTP step:
//
//   an installed extension   the content-hash gate is the sharp edge. A hash the
//                            port computes differently disables every installed
//                            extension on cutover morning, silently, because a
//                            drifted extension is a REFUSAL rather than an error.
//   a downloaded model       a presence check that disagrees costs the user a
//                            multi-GB re-download of a file already on disk.
//
// The extension case installs for real rather than planting the registry row:
// the fixture it plants is a dependency-free extension DIRECTORY, and the Deno
// core is then asked to install it over HTTP, which is what computes the hash
// this case is about. Only the raw directory is planted, so the property under
// test is still produced by the oracle rather than by the harness.
//
// The model case has no such option: a real download is multi-gigabyte and the
// suite must never make one (the repo's no-non-consented-network rule, and
// simple practicality). What is planted there is the END STATE of a download,
// and the property under test is not the transfer but whether both cores agree
// the resulting file is present. See D-168 for why that is the honest split.

import type { Case } from "../../runner/types.ts";

export const cases: Case[] = [
  {
    id: "f6.cutover.the-pin-survives-the-swap",
    package: "F6",
    routes: ["GET /api/v1/health"],
    auth: "none",
    isolate: true,
    steps: [
      {
        request: { method: "GET", path: "/api/v1/health" },
        expect: { status: 200, bodyContains: { status: "ok", uptimeMs: "<duration>" } },
      },
      {
        // The cert is regenerated on every boot; the KEY is sealed in the
        // secrets vault. Two independent implementations must unseal the same
        // vault and derive the same base64(SHA-256(SPKI)) from it, or every
        // already-paired client is holding a pin that no longer matches and is
        // locked out with no diagnosis beyond "re-pair". D-048 is what this
        // looks like when it goes wrong quietly: an 18-byte JWK difference that
        // three passes read as placeholder noise.
        handoff: "swap",
        request: { method: "GET", path: "/api/v1/health" },
        expect: {
          status: 200,
          pinUnchanged: true,
          bodyContains: { status: "ok", uptimeMs: "<duration>" },
        },
      },
    ],
  },
  {
    id: "f7.cutover.the-bearer-survives-the-swap",
    package: "F7",
    routes: ["GET /api/v1/pairing/clients"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: { method: "GET", path: "/api/v1/pairing/clients" },
        expect: {
          status: 200,
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
        // Pairing completed against the Deno core, over a real CPace handshake
        // folding the pin derived above. Two surfaces ride in this one step: the
        // bearer authenticating proves the Rust core reads the token HASH the
        // Deno core wrote to SQLite, and the id matching proves it reads the
        // client ROW. If either moved, a paired device comes back as a stranger
        // on the morning of the cutover.
        handoff: "swap",
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
  {
    id: "d2.cutover.a-session-written-by-deno-reads-on-the-port",
    package: "D2",
    routes: [
      "POST /api/v1/sessions",
      "POST /api/v1/sessions/:id/messages",
      "GET /api/v1/sessions/:id",
      "GET /api/v1/sessions",
    ],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "written by deno" } },
        expect: {
          status: 200,
          body: {
            id: "<id>",
            ownerClientId: "<id>",
            title: "written by deno",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
            tokenUsage: "<absent>",
            temporary: "<absent>",
          },
        },
        capture: { sid: "$.id", owner: "$.ownerClientId" },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "user", content: "does this survive the port?" },
        },
        expect: { status: 200, body: { id: "<id>", ord: 0 } },
        capture: { mid: "$.id" },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/sessions/${sid}/messages",
          body: { role: "assistant", content: "that is what this case is for" },
        },
        expect: { status: 200, body: { id: "<id>", ord: 1 } },
        capture: { mid2: "$.id" },
      },
      {
        // Sessions are JSON files under `<home>/sessions/`, a different
        // persistence path from the pin (the sealed vault) and the bearer
        // (SQLite), which is why this case is here rather than being assumed to
        // follow from those two. Ids, ordinals, and content are all asserted:
        // an id regenerated on read would still list, and the client's open
        // session would silently become a different one.
        handoff: "swap",
        request: { method: "GET", path: "/api/v1/sessions/${sid}" },
        expect: {
          status: 200,
          pinUnchanged: true,
          body: {
            id: "${sid}",
            ownerClientId: "${owner}",
            title: "written by deno",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
            tokenUsage: "<absent>",
            temporary: "<absent>",
            messages: [
              {
                id: "${mid}",
                role: "user",
                content: "does this survive the port?",
                ord: 0,
                createdAtMs: "<ms>",
              },
              {
                id: "${mid2}",
                role: "assistant",
                content: "that is what this case is for",
                ord: 1,
                createdAtMs: "<ms>",
              },
            ],
          },
        },
      },
      {
        // The list projection is computed rather than stored, so it exercises
        // the port's own summariser over the oracle's rows.
        request: { method: "GET", path: "/api/v1/sessions" },
        expect: {
          status: 200,
          body: [
            {
              id: "${sid}",
              title: "written by deno",
              createdAtMs: "<ms>",
              updatedAtMs: "<ms>",
              messageCount: 2,
              summary: [
                { role: "user", text: "does this survive the port?" },
                { role: "agent", text: "that is what this case is for" },
              ],
            },
          ],
        },
      },
    ],
  },
  {
    // Three stores in one case, because settings are not one store and a case
    // covering only the shared half would read as covering all of it:
    //
    //   `llm.gpuLayers`     settings.json, a file
    //   `llm.temperature`   the client_settings table, a SQLite row PER CLIENT
    //   the secret          the sealed vault, reached by enumeration
    //
    // The overlay half is the strongest assertion here and the reason two
    // clients are used. Those rows are keyed by the client id the DENO core
    // minted during pairing, so this fails if the port reads the table with any
    // other key: both clients would converge onto one value, or neither would
    // see its own. A single-client version of this case cannot tell those apart
    // from success.
    id: "d1.cutover.all-three-settings-stores-survive-the-swap",
    package: "D1",
    routes: [
      "PATCH /api/v1/settings",
      "GET /api/v1/settings",
      "PUT /api/v1/settings/secrets/:name",
      "GET /api/v1/settings/secrets",
    ],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "PATCH",
          path: "/api/v1/settings",
          body: { "llm.gpuLayers": 12, "llm.temperature": 0.2 },
        },
        expect: { status: 200, bodyContains: { "llm.gpuLayers": 12, "llm.temperature": 0.2 } },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/settings",
          body: { "llm.temperature": 0.9 },
          auth: "paired:other",
        },
        expect: { status: 200, bodyContains: { "llm.temperature": 0.9 } },
      },
      {
        // The vault already has one entry the port must read (`tls-key`, proven
        // by the pin case above). This adds the half that case cannot reach: the
        // NAME enumeration, filtered to the settings schema's password fields.
        // A port that unseals correctly but enumerates differently reports no
        // secrets configured, and the user re-enters every API key they own.
        request: {
          method: "PUT",
          path: "/api/v1/settings/secrets/llm.external.apiKey",
          body: { value: "sk-conformance-must-not-leak" },
        },
        expect: { status: 204 },
      },
      {
        handoff: "swap",
        request: { method: "GET", path: "/api/v1/settings" },
        expect: {
          status: 200,
          pinUnchanged: true,
          bodyContains: {
            "llm.gpuLayers": 12,
            "llm.temperature": 0.2,
            "llm.provider": "external",
          },
        },
      },
      {
        request: { method: "GET", path: "/api/v1/settings", auth: "paired:other" },
        expect: { status: 200, bodyContains: { "llm.gpuLayers": 12, "llm.temperature": 0.9 } },
      },
      {
        // The name comes back and the value does not, which is the same
        // contract the non-cutover case asserts. Exact, so a value leaking
        // beside the name fails here too.
        request: { method: "GET", path: "/api/v1/settings/secrets" },
        expect: { status: 200, body: { names: ["llm.external.apiKey"] } },
      },
    ],
  },
  {
    // Memories are the one store where the files and the index can disagree,
    // which is why this case rescans rather than only reading.
    //
    // `contentHash` is the assertion that matters and it is HARD-CODED, per the
    // rule in d5-memories.ts: capturing it would make the assertion agree with
    // whatever each core produced, which is the one thing it must not do. It is
    // sha256 of exactly the bytes on disk. If the port hashes differently, or
    // writes different bytes for the same body, every memory in the corpus reads
    // as changed on first boot: `summaryStale` flips true and the whole corpus
    // is re-summarised and re-embedded, which on a real install is a long stall
    // that looks like a hang.
    id: "d5.cutover.a-memory-written-by-deno-reads-and-rescans-clean-on-the-port",
    package: "D5",
    routes: ["POST /api/v1/memories", "GET /api/v1/memories/:id", "POST /api/v1/memories/rescan"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/memories",
          body: { title: "Meeting Notes: Q3!", content: "# Notes\nhello" },
        },
        expect: {
          status: 201,
          bodyContains: {
            id: "<id>",
            kind: "knowledge",
            filename: "meeting-notes-q3.md",
            contentHash: "5d5b45b451636cc44a1118c2858e7f8291c4e14298ddd534e14bc5e8aa83336e",
            summaryStale: true,
          },
        },
        capture: { mid: "$.id" },
      },
      {
        handoff: "swap",
        request: { method: "GET", path: "/api/v1/memories/${mid}" },
        expect: {
          status: 200,
          pinUnchanged: true,
          bodyContains: {
            id: "${mid}",
            kind: "knowledge",
            title: "Meeting Notes: Q3!",
            filename: "meeting-notes-q3.md",
            content: "# Notes\nhello",
            contentHash: "5d5b45b451636cc44a1118c2858e7f8291c4e14298ddd534e14bc5e8aa83336e",
            enabled: true,
            provider: "user",
          },
        },
      },
      {
        // Zeros are the whole assertion. The row and the file were written by
        // the OTHER core, so this is the only place the port's reconciler is
        // pointed at a tree it did not produce. A non-zero `changed` means the
        // port disagrees with the oracle about the bytes or the hash; a non-zero
        // `removed` plus `added` means it disagrees about identity and every
        // memory silently gets a new id, breaking anything that referenced one.
        request: { method: "POST", path: "/api/v1/memories/rescan" },
        expect: { status: 200, body: { added: 0, removed: 0, changed: 0 } },
      },
    ],
  },
  {
    // The derived-value case, and the one whose failure is silent.
    //
    // `nextRunAtMs` is not read from disk. Both cores recompute it from the
    // schedule and their own local wall clock, so the port can persist every
    // stored field perfectly and still arm the wrong instant, or none. The
    // schedule then simply never fires, which no user reports as a bug and no
    // other case here would catch: the row lists fine and reads back fine.
    //
    // `<ms-future>` rather than a literal, for the reason d10-scheduled-prompts.ts
    // gives: the value depends on the machine's timezone and on when in the week
    // the suite runs. What is contractual is that the port re-armed it at all,
    // in epoch MILLISECONDS, in the future. That last part is what catches the
    // seconds-instead-of-ms mistake, which would arm every schedule in 1970 and
    // fire them all at once on the first boot after cutover.
    id: "d10.cutover.a-schedule-written-by-deno-is-re-armed-by-the-port",
    package: "D10",
    routes: ["POST /api/v1/scheduled-prompts", "GET /api/v1/scheduled-prompts"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/scheduled-prompts",
          body: {
            title: "Weekly review",
            instruction: "Summarise what I worked on this week.",
            schedule: { kind: "weekly", weekdays: [1], hour: 9, minute: 0 },
            runMissed: false,
          },
        },
        expect: {
          status: 201,
          bodyContains: { id: "<id>", enabled: true, nextRunAtMs: "<ms-future>" },
        },
        capture: { sid: "$.id", owner: "$.ownerClientId" },
      },
      {
        handoff: "swap",
        request: { method: "GET", path: "/api/v1/scheduled-prompts" },
        expect: {
          status: 200,
          pinUnchanged: true,
          body: {
            scheduledPrompts: [
              {
                id: "${sid}",
                ownerClientId: "${owner}",
                title: "Weekly review",
                instruction: "Summarise what I worked on this week.",
                schedule: { kind: "weekly", weekdays: [1], hour: 9, minute: 0 },
                runMissed: false,
                enabled: true,
                // Never ran under either core, and ABSENT rather than null is
                // how the client tells that from a schedule that has run.
                lastRunAtMs: "<absent>",
                nextRunAtMs: "<ms-future>",
                createdAtMs: "<ms>",
                updatedAtMs: "<ms>",
              },
            ],
          },
        },
      },
    ],
  },
  {
    // The content-hash gate, which is the one cutover failure that is a REFUSAL
    // rather than an error. `verify_all_on_boot` rehashes every installed
    // extension at startup and marks a mismatch as `drift`; a drifted extension
    // keeps its row and stops offering tools. So a port that walks the install
    // directory in a different order, or hashes different bytes, produces a
    // core that boots clean, reports no error, and has silently disarmed every
    // extension the user installed.
    //
    // The extension is dependency-free ON PURPOSE. Installing one with
    // dependencies needs the `deno` worker runtime, which is a separate
    // consent-gated download and is absent from a conformance home, so the
    // shipped extensions can only ever reach `downloaded` here. A manifest with
    // no `deno.json` skips that path entirely
    // (`install/runner.rs:667`, `if !has_deps`), takes the content hash and
    // marks the row `installed`, which is exactly the state this case needs.
    id: "d12.cutover.an-extension-installed-by-deno-still-verifies-on-the-port",
    package: "D12",
    routes: ["POST /api/v1/extensions/download", "GET /api/v1/extensions"],
    auth: "paired",
    isolate: true,
    watch: ["extension.install_done"],
    plant: [
      {
        path: "cutover-fixture/loca.json",
        content: JSON.stringify(
          {
            name: "cutover-hash-fixture",
            displayName: "Cutover Hash Fixture",
            description: "A dependency-free extension, planted to test hash agreement.",
            tools: [
              {
                name: "noop",
                description: "Does nothing at all.",
                parameters: { type: "object", properties: {} },
                function: "main.ts#noop",
              },
            ],
          },
          null,
          2,
        ),
      },
      // A second file, so the hash covers a WALK rather than a single known
      // path. A port that hashed only `loca.json` would pass a one-file
      // fixture and drift on every real extension.
      { path: "cutover-fixture/main.ts", content: "export function noop() {}\n" },
    ],
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/extensions/download",
          body: {
            source: "local",
            path: "${home}/cutover-fixture",
            slug: "cutover-hash-fixture",
          },
        },
        expect: {
          status: 200,
          bodyContains: { jobId: "<id>" },
          // The install is a job, so the listing below would race it without
          // this. `ok: true` rather than just the frame's arrival: a failed
          // install also emits `install_done`, and asserting only the kind
          // would let this case proceed to a listing that legitimately has no
          // row.
          // Every key, because a frame is matched whole rather than as a
          // subset. `code: 0` is the process exit status the install reports
          // and is contractual alongside `ok`.
          frames: [
            {
              kind: "extension.install_done",
              jobId: "<id>",
              id: "cutover-hash-fixture",
              ok: true,
              code: 0,
            },
          ],
        },
      },
      {
        request: { method: "GET", path: "/api/v1/extensions" },
        expect: {
          status: 200,
          // POSITIONAL: `bodyContains` on an array matches index by index, so
          // `{}` is an explicit don't-care for the seeded Built-In row that
          // sorts ahead of the fixture. Not a subset match; a bare
          // one-element array here would assert about Built-In.
          bodyContains: [
            {},
            {
              id: "cutover-hash-fixture",
              source: "local",
              // `installed`, not `downloaded`. This is the assertion that makes
              // the swap below mean anything: `verify_all_on_boot` SKIPS every
              // status but this one, so a case that cut over a `downloaded` row
              // would verify nothing and still pass.
              status: "installed",
              contentHash: "<sha256>",
            },
          ],
        },
        // Captured rather than matched as `<sha256>` on the far side. A
        // placeholder would accept ANY well-formed hash, which is precisely the
        // failure this case exists to catch: the port computing a different one.
        //
        // Index 1 because the listing is ordered by `display_name COLLATE
        // NOCASE ASC` (`registry.rs:841`) and the three rows are Built-In,
        // Cutover Hash Fixture, Samples. **A wrong index here cannot produce a
        // false pass**: it would capture a SEEDED row, whose `contentHash` is
        // empty by definition, and the swap step below demands that value on
        // the fixture's own row, which has a real hash. The failure mode is a
        // red this comment explains, not a green that means nothing.
        capture: { hash: "$[1].contentHash" },
      },
      {
        handoff: "swap",
        request: { method: "GET", path: "/api/v1/extensions" },
        expect: {
          status: 200,
          pinUnchanged: true,
          bodyContains: [
            {},
            {
              id: "cutover-hash-fixture",
              source: "local",
              // Still `installed`. `drift` here is the whole failure mode, and
              // it is what a hash disagreement produces.
              status: "installed",
              // The literal hash the ORACLE computed. Byte-for-byte, across two
              // independent implementations of the directory walk.
              contentHash: "${hash}",
            },
          ],
        },
      },
    ],
  },
  {
    // A model the user already downloaded must still read as present after the
    // cutover. If it does not, the requirements flow offers the download again
    // and the user pays for multiple gigabytes they already have, which is the
    // most expensive silent cutover failure available.
    //
    // What is planted is the END STATE of a download rather than a download,
    // for two reasons: a real transfer is multi-gigabyte, and the repo's
    // absolute rule is that the suite makes no non-consented network request.
    // The property under test is not the transfer. It is whether two
    // implementations of `resolve_hf_path` + the on-disk walk agree about what
    // is there, and the planted file exercises exactly that.
    //
    // `LOCA_CORE_HOME` roots `models/` inside the throwaway home
    // (`paths.rs:69-76`), so this cannot touch the developer's real
    // `~/.loca/models`. That containment is load-bearing: the models directory
    // is otherwise SHARED across channels by design.
    id: "d6.cutover.a-model-downloaded-by-deno-is-still-present-on-the-port",
    package: "D6",
    routes: ["GET /api/v1/models"],
    auth: "paired",
    isolate: true,
    plant: [
      // The layout `@user/repo/branch/file` resolves to
      // (`models/manager.rs:37-56`). The nesting is the point: a port that
      // flattened it, or that joined the branch as a directory, would list
      // nothing here while the file sits on disk.
      { path: "models/TheOrg/The-Model-GGUF/the-model.Q4_K_M.gguf", bytes: 2048 },
    ],
    steps: [
      {
        request: { method: "GET", path: "/api/v1/models" },
        expect: {
          status: 200,
          body: [
            {
              absPath: "<home>/models/TheOrg/The-Model-GGUF/the-model.Q4_K_M.gguf",
              relPath: "TheOrg/The-Model-GGUF/the-model.Q4_K_M.gguf",
              // Literal, not a placeholder. The size is what the client renders
              // and what the storage screen sums, and a port reporting blocks
              // rather than bytes would pass any placeholder.
              sizeBytes: 2048,
              source: "<string>",
            },
          ],
        },
      },
      {
        handoff: "swap",
        request: { method: "GET", path: "/api/v1/models" },
        expect: {
          status: 200,
          pinUnchanged: true,
          // The identical listing, from the other implementation. An empty
          // array here is the defect: the file is on disk and the core cannot
          // see it, so every requirement it satisfies reopens.
          body: [
            {
              absPath: "<home>/models/TheOrg/The-Model-GGUF/the-model.Q4_K_M.gguf",
              relPath: "TheOrg/The-Model-GGUF/the-model.Q4_K_M.gguf",
              sizeBytes: 2048,
              source: "<string>",
            },
          ],
        },
      },
    ],
  },
];
