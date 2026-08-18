// D5: memories, and the ten routes under `/api/v1/memories`.
//
// The strongest assertion here is `contentHash`. It is `sha256` of exactly the
// bytes on disk, so it is deterministic across cores and across runs: both cores
// must produce the same 64 hex characters for the same body. Where a per-run id
// can only be `<id>`, these can pin a real value, and a divergence in the
// hashing or in what got written shows up as a literal mismatch rather than as a
// shrug.
//
// Per D-029 every per-run value is asserted with a placeholder rather than left
// out, so differential mode has nothing unasserted left to report as noise.
//
// Each case creates whatever it reads and cleans up after itself: captures do
// not cross case boundaries (the runner scopes `vars` per case), and cases in a
// scenario share one core, so a case that left a memory behind would change what
// the next one sees.
//
// What these deliberately do NOT assert, and why:
//
//   - Nothing about `summary` or `embedding` beyond what frontmatter seeds. Both
//     are written by the background indexer, which needs a language model the
//     harness does not provide and an embedding model neither core has here. Both
//     cores therefore report `summaryStale: true` for anything not seeded, which
//     IS asserted; the index pass itself is covered by Rust tests against a
//     scripted generator.
//   - Rescan adopting a hand-placed file. No conformance step can write into the
//     core's memories directory, so adoption (a new file, a new skill folder, a
//     title collision) is covered by Rust tests where a file can be dropped in
//     directly. What is asserted here is the answer's shape and that a settled
//     tree reports zeros.
//   - Extension-provided memories, which need an installed extension (D12).

import type { Case } from "../runner/types.ts";

/** sha256 of the exact bytes each case writes. Hard-coded rather than captured:
 *  capturing would make the assertion agree with whatever each core produced,
 *  which is the one thing it must not do. */
const EMPTY = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const NOTES = "5d5b45b451636cc44a1118c2858e7f8291c4e14298ddd534e14bc5e8aa83336e";
const REWRITTEN = "04d96d8a91ea74aa0ec9adb2f390d4f025b3a4517bae57226f0903962de682b0";

export const cases: Case[] = [
  {
    id: "d5.memories.lifecycle",
    package: "D5",
    routes: [
      "POST /api/v1/memories",
      "GET /api/v1/memories",
      "GET /api/v1/memories/:id",
      "PATCH /api/v1/memories/:id",
      "POST /api/v1/memories/:id/reindex",
      "DELETE /api/v1/memories/:id",
    ],
    auth: "paired",
    scenario: "d5",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/memories",
          body: { title: "Meeting Notes: Q3!", content: "# Notes\nhello" },
        },
        expect: {
          status: 201,
          body: {
            id: "<id>",
            kind: "knowledge",
            title: "Meeting Notes: Q3!",
            // The slug rules on the wire: lowercased, every punctuation run
            // collapsed to one dash, trimmed, `.md` because it is knowledge.
            filename: "meeting-notes-q3.md",
            contentHash: NOTES,
            // Never indexed, so stale. Present and true, not absent.
            summaryStale: true,
            provider: "user",
            enabled: true,
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
            content: "# Notes\nhello",
            // Knowledge carries no skill fields at all, and absent is not the
            // same as empty (D-028): a client reading `files: []` here would
            // draw a bundled-files list that cannot exist.
            summary: "<absent>",
            suggestedTools: "<absent>",
            files: "<absent>",
          },
        },
        capture: { mid: "$.id" },
      },
      {
        // The listing is metadata only: same row, no `content`.
        request: { method: "GET", path: "/api/v1/memories" },
        expect: {
          status: 200,
          body: {
            memories: [
              {
                id: "${mid}",
                kind: "knowledge",
                title: "Meeting Notes: Q3!",
                filename: "meeting-notes-q3.md",
                contentHash: NOTES,
                summaryStale: true,
                provider: "user",
                enabled: true,
                createdAtMs: "<ms>",
                updatedAtMs: "<ms>",
                content: "<absent>",
                summary: "<absent>",
                suggestedTools: "<absent>",
                files: "<absent>",
              },
            ],
          },
        },
      },
      {
        request: { method: "GET", path: "/api/v1/memories/${mid}" },
        expect: {
          status: 200,
          bodyContains: {
            id: "${mid}",
            content: "# Notes\nhello",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
          },
        },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/memories/${mid}",
          body: { title: "Renamed", content: "rewritten", enabled: false },
        },
        expect: {
          status: 200,
          bodyContains: {
            id: "${mid}",
            title: "Renamed",
            content: "rewritten",
            enabled: false,
            contentHash: REWRITTEN,
            summaryStale: true,
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
            // The rename does not move the file. Names only have to be unique,
            // and moving one would break every external reference for no gain.
            filename: "meeting-notes-q3.md",
          },
        },
      },
      {
        // An empty patch is a 200 no-op rather than a 400.
        request: { method: "PATCH", path: "/api/v1/memories/${mid}", body: {} },
        expect: {
          status: 200,
          bodyContains: {
            id: "${mid}",
            title: "Renamed",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
          },
        },
      },
      {
        // `{ ok: true }` with a 200, not a 204: four of the ten routes answer
        // this way and the client's wrappers expect a body.
        request: { method: "POST", path: "/api/v1/memories/${mid}/reindex" },
        expect: { status: 200, body: { ok: true } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/memories/${mid}" },
        expect: { status: 200, body: { ok: true } },
      },
      {
        request: { method: "GET", path: "/api/v1/memories/${mid}" },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: { method: "GET", path: "/api/v1/memories" },
        expect: { status: 200, body: { memories: [] } },
      },
    ],
  },
  {
    id: "d5.memories.title-conflict-and-slug-suffix",
    package: "D5",
    routes: ["POST /api/v1/memories", "DELETE /api/v1/memories/:id"],
    auth: "paired",
    scenario: "d5",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/memories", body: { title: "Notes" } },
        expect: {
          status: 201,
          bodyContains: {
            id: "<id>",
            filename: "notes.md",
            contentHash: EMPTY,
            content: "",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
          },
        },
        capture: { first: "$.id" },
      },
      {
        // Tools address memories by the title they saw in the prompt, which the
        // model may re-case, so "notes" and "Notes" cannot both exist.
        request: { method: "POST", path: "/api/v1/memories", body: { title: "notes" } },
        expect: { status: 409, bodyContains: { error: { code: "conflict" } } },
      },
      {
        // A different title that slugs the same still gets a filename, suffixed.
        request: { method: "POST", path: "/api/v1/memories", body: { title: "Notes!" } },
        expect: {
          status: 201,
          bodyContains: {
            id: "<id>",
            filename: "notes-2.md",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
          },
        },
        capture: { second: "$.id" },
      },
      { request: { method: "DELETE", path: "/api/v1/memories/${first}" }, expect: { status: 200 } },
      {
        request: { method: "DELETE", path: "/api/v1/memories/${second}" },
        expect: { status: 200 },
      },
    ],
  },
  {
    id: "d5.memories.rejects-a-bad-body",
    package: "D5",
    routes: ["POST /api/v1/memories", "PATCH /api/v1/memories/:id"],
    auth: "paired",
    scenario: "d5",
    steps: [
      // The bodies are `.strict()` in `@loca/shared`, so an unknown key is
      // refused rather than ignored. This is the one place the wire is
      // deliberately closed, and D-031 settled that the generator must not open
      // it on the way to Rust.
      {
        request: {
          method: "POST",
          path: "/api/v1/memories",
          body: { title: "Fine", unexpected: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/memories", body: { title: "" } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/memories", body: {} },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/memories",
          body: { title: "Fine", kind: "recipe" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/memories", body: { title: "Patchable" } },
        expect: {
          status: 201,
          bodyContains: {
            id: "<id>",
            title: "Patchable",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
          },
        },
        capture: { pid: "$.id" },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/memories/${pid}",
          body: { unexpected: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/memories/${pid}" },
        expect: { status: 200, body: { ok: true } },
      },
    ],
  },
  {
    // A rejected patch must leave the memory exactly as it was.
    //
    // The handler applies `enabled` first and `title` second, so a body pairing
    // a valid `enabled` with an empty `title` half-applies unless `.min(1)` is
    // refused on the way in. It is, but only because the schema now lives in
    // `@loca/shared/validation`: route-local, the bridge never saw it, Rust
    // deserialized the empty title and left `enabled` written before the rename
    // failed. Status and code alone cannot tell the two apart, which is why the
    // read-back is the assertion. See D-093.
    id: "d5.memories.a-rejected-patch-applies-nothing",
    package: "D5",
    routes: ["POST /api/v1/memories", "PATCH /api/v1/memories/:id", "DELETE /api/v1/memories/:id"],
    auth: "paired",
    scenario: "d5",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/memories", body: { title: "Untouched" } },
        expect: {
          status: 201,
          bodyContains: {
            id: "<id>",
            title: "Untouched",
            enabled: true,
            content: "",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
          },
        },
        capture: { rid: "$.id" },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/memories/${rid}",
          body: { enabled: false, title: "" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "GET", path: "/api/v1/memories/${rid}" },
        expect: {
          status: 200,
          bodyContains: {
            id: "${rid}",
            title: "Untouched",
            enabled: true,
            content: "",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
          },
        },
      },
      {
        request: { method: "DELETE", path: "/api/v1/memories/${rid}" },
        expect: { status: 200, body: { ok: true } },
      },
    ],
  },
  {
    id: "d5.memories.unknown-id-is-not-found",
    package: "D5",
    routes: [
      "GET /api/v1/memories/:id",
      "PATCH /api/v1/memories/:id",
      "DELETE /api/v1/memories/:id",
      "POST /api/v1/memories/:id/reindex",
      "GET /api/v1/memories/:id/files/:name",
    ],
    auth: "paired",
    scenario: "d5",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/memories/01ZZZZZZZZZZZZZZZZZZZZZZZZ" },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/memories/01ZZZZZZZZZZZZZZZZZZZZZZZZ",
          body: { title: "X" },
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/memories/01ZZZZZZZZZZZZZZZZZZZZZZZZ" },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        // The reindex route reads the memory first, so an unknown id is a 404
        // rather than a pass silently scheduled over nothing.
        request: {
          method: "POST",
          path: "/api/v1/memories/01ZZZZZZZZZZZZZZZZZZZZZZZZ/reindex",
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: {
          method: "GET",
          path: "/api/v1/memories/01ZZZZZZZZZZZZZZZZZZZZZZZZ/files/x.md",
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
    ],
  },
  {
    id: "d5.memories.skill-frontmatter-and-bundled-files",
    package: "D5",
    routes: [
      "POST /api/v1/memories",
      "GET /api/v1/memories/:id",
      "PUT /api/v1/memories/:id/files/:name",
      "GET /api/v1/memories/:id/files/:name",
      "DELETE /api/v1/memories/:id/files/:name",
      "DELETE /api/v1/memories/:id",
    ],
    auth: "paired",
    scenario: "d5-skills",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/memories",
          body: {
            kind: "skill",
            title: "File Bug",
            content:
              "---\ndescription: Files a bug report\nsuggested-tools: [web_search, write_memory]\n---\n# Steps\ndo the thing",
          },
        },
        expect: {
          status: 201,
          bodyContains: {
            kind: "skill",
            // A folder, so no `.md`.
            filename: "file-bug",
            // The frontmatter seeds the summary, and it is NOT stale: it was
            // derived from exactly this content, so the indexer has nothing to
            // do until the body changes.
            summary: "Files a bug report",
            summaryStale: false,
            suggestedTools: ["web_search", "write_memory"],
            // Present and empty for a skill with no bundled files, where
            // knowledge omits the field entirely.
            files: [],
            id: "<id>",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
          },
        },
        capture: { skill: "$.id" },
      },
      {
        request: {
          method: "PUT",
          path: "/api/v1/memories/${skill}/files/template.md",
          body: { content: "bug template" },
        },
        expect: { status: 200, body: { ok: true } },
      },
      {
        request: { method: "GET", path: "/api/v1/memories/${skill}/files/template.md" },
        expect: { status: 200, body: { content: "bug template" } },
      },
      {
        request: { method: "GET", path: "/api/v1/memories/${skill}" },
        expect: {
          status: 200,
          bodyContains: {
            id: "${skill}",
            files: ["template.md"],
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
          },
        },
      },
      {
        request: { method: "DELETE", path: "/api/v1/memories/${skill}/files/template.md" },
        expect: { status: 200, body: { ok: true } },
      },
      {
        // Not idempotent, on purpose: a silent success would hide a typo.
        request: { method: "DELETE", path: "/api/v1/memories/${skill}/files/template.md" },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: { method: "GET", path: "/api/v1/memories/${skill}/files/missing.md" },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        // SKILL.md is the instruction body, edited through PATCH. Refused in any
        // casing, because on a case-insensitive filesystem a bundled `skill.md`
        // would resolve to it and overwrite it.
        request: {
          method: "PUT",
          path: "/api/v1/memories/${skill}/files/SKILL.md",
          body: { content: "hijacked" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "PUT",
          path: "/api/v1/memories/${skill}/files/skill.md",
          body: { content: "hijacked" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // Percent-encoded so the traversal survives as one path segment and
        // reaches the handler as `../escape`.
        request: {
          method: "PUT",
          path: "/api/v1/memories/${skill}/files/..%2Fescape",
          body: { content: "hijacked" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/memories/${skill}" },
        expect: { status: 200, body: { ok: true } },
      },
    ],
  },
  {
    id: "d5.memories.a-skill-without-frontmatter-omits-its-optional-fields",
    package: "D5",
    routes: ["POST /api/v1/memories", "DELETE /api/v1/memories/:id"],
    auth: "paired",
    scenario: "d5-skills",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/memories",
          body: { kind: "skill", title: "Bare", content: "# Steps" },
        },
        expect: {
          status: 201,
          bodyContains: {
            kind: "skill",
            files: [],
            // No frontmatter: no summary at all, and no tools field either.
            summary: "<absent>",
            suggestedTools: "<absent>",
            summaryStale: true,
            id: "<id>",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
          },
        },
        capture: { bare: "$.id" },
      },
      {
        request: { method: "DELETE", path: "/api/v1/memories/${bare}" },
        expect: { status: 200, body: { ok: true } },
      },
    ],
  },
  {
    id: "d5.memories.a-bundled-file-on-knowledge-is-refused",
    package: "D5",
    routes: ["PUT /api/v1/memories/:id/files/:name"],
    auth: "paired",
    scenario: "d5-skills",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/memories",
          body: { title: "Plain Note", content: "body" },
        },
        expect: {
          status: 201,
          bodyContains: {
            id: "<id>",
            kind: "knowledge",
            createdAtMs: "<ms>",
            updatedAtMs: "<ms>",
          },
        },
        capture: { note: "$.id" },
      },
      {
        request: {
          method: "PUT",
          path: "/api/v1/memories/${note}/files/x.md",
          body: { content: "y" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/memories/${note}" },
        expect: { status: 200, body: { ok: true } },
      },
    ],
  },
  {
    id: "d5.memories.rescan-on-a-settled-tree",
    package: "D5",
    routes: ["POST /api/v1/memories/rescan"],
    auth: "paired",
    scenario: "d5-skills",
    steps: [
      {
        // Everything in this scenario was created through the API, so the rows
        // and the files already agree and a rescan changes nothing. Adoption
        // needs a file dropped in behind the core's back, which no conformance
        // step can do; it is covered by Rust tests.
        request: { method: "POST", path: "/api/v1/memories/rescan" },
        expect: { status: 200, body: { added: 0, removed: 0, changed: 0 } },
      },
    ],
  },
  {
    id: "d5.memories.requires-a-paired-client",
    package: "D5",
    routes: [
      "GET /api/v1/memories",
      "POST /api/v1/memories",
      "POST /api/v1/memories/rescan",
      "GET /api/v1/memories/:id",
    ],
    auth: "none",
    scenario: "d5-skills",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/memories" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/memories", body: { title: "X" } },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/memories/rescan" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
      {
        request: { method: "GET", path: "/api/v1/memories/anything" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
];
