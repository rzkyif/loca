// D1: the settings store and its secrets sub-resource.
//
// Two stores sit behind `/api/v1/settings`: the shared `core` store every
// client of this core sees, and the per-client `client-on-core` overlay that
// only its owner sees. The cases below assert the difference from the outside,
// which is the only place it is observable.
//
// Written against the contract (@loca/shared's settings schema and
// `validateSettingsPatch`), not against either core's output. Every rejection
// here names a specific rule: an unknown key, a destination the path does not
// accept, a wrong type, a value outside a declared range, a value outside a
// select's options, a regex rule, and a secret that belongs in the vault.
//
// The `settings.updated` broadcast is deliberately absent. Every one of those
// frames is triggered by an HTTP request, and a WS step here is self-contained
// (upgrade, send, expect, close), so no case can observe one. It is covered by
// the Rust store suite against a two-connection FrameBus instead.
//
// Note that the harness seeds a real `settings.json` (harness/home.ts) pointing
// the providers at the mock upstream, so GET is never empty here and these
// cases assert with `bodyContains`.

import type { Case } from "../runner/types.ts";

const SETTINGS = "/api/v1/settings";

export const cases: Case[] = [
  {
    // The seeded file is an "existing install" in miniature: the core reads it
    // as it stands rather than rewriting it from defaults.
    id: "d1.settings.reads-an-existing-file",
    package: "D1",
    routes: ["GET /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: { method: "GET", path: SETTINGS },
        expect: {
          status: 200,
          bodyContains: { "llm.provider": "external", "llm.external.model": "mock-model" },
        },
      },
    ],
  },
  {
    id: "d1.settings.requires-a-bearer",
    package: "D1",
    routes: ["GET /api/v1/settings"],
    auth: "none",
    scenario: "d1",
    steps: [
      {
        request: { method: "GET", path: SETTINGS },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    // A PATCH returns the merged effective view, not just what it wrote.
    id: "d1.settings.patch-persists-and-returns-the-merged-view",
    package: "D1",
    routes: ["PATCH /api/v1/settings", "GET /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: { method: "PATCH", path: SETTINGS, body: { "llm.contextSize": 8192 } },
        expect: {
          status: 200,
          bodyContains: { "llm.contextSize": 8192, "llm.provider": "external" },
        },
      },
      {
        request: { method: "GET", path: SETTINGS },
        expect: { status: 200, bodyContains: { "llm.contextSize": 8192 } },
      },
    ],
  },
  {
    // A null is a deletion: the key leaves the sparse store and reverts to the
    // schema default, which is what makes a changed default reach an existing
    // install. The response must not carry it at all, since the store is sparse
    // and the default lives in the schema both sides share.
    id: "d1.settings.a-null-reverts-a-key-to-its-default",
    package: "D1",
    routes: ["PATCH /api/v1/settings", "GET /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: { method: "PATCH", path: SETTINGS, body: { "llm.threads": 8 } },
        expect: { status: 200, bodyContains: { "llm.threads": 8 } },
      },
      {
        request: { method: "PATCH", path: SETTINGS, body: { "llm.threads": null } },
        expect: { status: 200, bodyContains: { "llm.threads": "<absent>" } },
      },
      {
        request: { method: "GET", path: SETTINGS },
        expect: { status: 200, bodyContains: { "llm.threads": "<absent>" } },
      },
    ],
  },
  {
    id: "d1.settings.rejects-an-unknown-key",
    package: "D1",
    routes: ["PATCH /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: { method: "PATCH", path: SETTINGS, body: { "ui.theme": "dark" } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // `server.bindHost` and `server.behindProxy` live in the same file and
        // are deliberately NOT schema fields, so that a paired client cannot
        // widen the bind host or clear the pairing posture over the API. They
        // are unknown keys here, and invisible to GET.
        request: { method: "PATCH", path: SETTINGS, body: { "server.bindHost": "0.0.0.0" } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "PATCH", path: SETTINGS, body: { "server.behindProxy": true } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "GET", path: SETTINGS },
        expect: {
          status: 200,
          bodyContains: { "server.bindHost": "<absent>", "server.behindProxy": "<absent>" },
        },
      },
    ],
  },
  {
    // A `client-on-client` key belongs in the Client's own file. The core never
    // reads it and must not start storing it.
    id: "d1.settings.rejects-a-client-only-key",
    package: "D1",
    routes: ["PATCH /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: { method: "PATCH", path: SETTINGS, body: { "appearance.theme": "dark" } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d1.settings.rejects-a-wrong-typed-value",
    package: "D1",
    routes: ["PATCH /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        // llm.host is a string field.
        request: { method: "PATCH", path: SETTINGS, body: { "llm.host": 1234 } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // llm.contextSize is a number field, and "8192" is not a number.
        request: { method: "PATCH", path: SETTINGS, body: { "llm.contextSize": "8192" } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    // The schema's own guards, all of which exist so a malformed value cannot
    // reach the core and flow into a sidecar's argv.
    id: "d1.settings.rejects-values-the-schema-refuses",
    package: "D1",
    routes: ["PATCH /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        // llm.host carries an IPv4 regex rule.
        request: { method: "PATCH", path: SETTINGS, body: { "llm.host": "not-an-ip" } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // llm.provider is a select over local | external.
        request: { method: "PATCH", path: SETTINGS, body: { "llm.provider": "banana" } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // memories.maxRelevant is a slider bounded at 1..10.
        request: { method: "PATCH", path: SETTINGS, body: { "memories.maxRelevant": 42 } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // llm.external.contextSize declares min 1.
        request: { method: "PATCH", path: SETTINGS, body: { "llm.external.contextSize": 0 } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // memories.list is an object_management field: a manager, not a value.
        request: { method: "PATCH", path: SETTINGS, body: { "memories.list": "x" } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // The provider settings the mock upstream depends on are untouched by
        // every rejection above.
        request: { method: "GET", path: SETTINGS },
        expect: { status: 200, bodyContains: { "llm.provider": "external" } },
      },
    ],
  },
  {
    // A secret-typed field's value belongs in the encrypted vault and must never
    // be written to settings.json, where it would be readable on disk and
    // returned to every client.
    id: "d1.settings.rejects-a-secret-typed-key",
    package: "D1",
    routes: ["PATCH /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: {
          method: "PATCH",
          path: SETTINGS,
          body: { "llm.external.apiKey": "sk-should-be-rejected" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "GET", path: SETTINGS },
        expect: { status: 200, bodyContains: { "llm.external.apiKey": "<absent>" } },
      },
    ],
  },
  {
    id: "d1.settings.rejects-a-non-object-body",
    package: "D1",
    routes: ["PATCH /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: { method: "PATCH", path: SETTINGS, body: [1, 2, 3] },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    // A `core` key is shared physical configuration: every paired client sees
    // the same value.
    id: "d1.settings.a-core-key-is-shared-between-clients",
    package: "D1",
    routes: ["PATCH /api/v1/settings", "GET /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: { method: "PATCH", path: SETTINGS, body: { "llm.gpuLayers": 12 } },
        expect: { status: 200, bodyContains: { "llm.gpuLayers": 12 } },
      },
      {
        request: { method: "GET", path: SETTINGS, auth: "paired:other" },
        expect: { status: 200, bodyContains: { "llm.gpuLayers": 12 } },
      },
    ],
  },
  {
    // A `client-on-core` key is one client's own inference preference. The core
    // applies it server-side for that client's turns, and another client must
    // neither see it nor converge onto it.
    id: "d1.settings.an-overlay-key-belongs-to-one-client",
    package: "D1",
    routes: ["PATCH /api/v1/settings", "GET /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: { method: "PATCH", path: SETTINGS, body: { "llm.temperature": 0.2 } },
        expect: { status: 200, bodyContains: { "llm.temperature": 0.2 } },
      },
      {
        request: { method: "GET", path: SETTINGS, auth: "paired:other" },
        expect: { status: 200, bodyContains: { "llm.temperature": "<absent>" } },
      },
      {
        request: {
          method: "PATCH",
          path: SETTINGS,
          body: { "llm.temperature": 0.9 },
          auth: "paired:other",
        },
        expect: { status: 200, bodyContains: { "llm.temperature": 0.9 } },
      },
      {
        // Each client keeps its own value, over the same shared store.
        request: { method: "GET", path: SETTINGS },
        expect: { status: 200, bodyContains: { "llm.temperature": 0.2 } },
      },
    ],
  },
  {
    // Values go into the vault and never come back out: the client learns which
    // names are configured and renders a placeholder from that.
    id: "d1.secrets.a-value-goes-in-and-only-its-name-comes-back",
    package: "D1",
    routes: [
      "PUT /api/v1/settings/secrets/:name",
      "GET /api/v1/settings/secrets",
      "DELETE /api/v1/settings/secrets/:name",
    ],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: {
          method: "PUT",
          path: "/api/v1/settings/secrets/llm.external.apiKey",
          body: { value: "sk-conformance-must-not-leak" },
        },
        expect: { status: 204 },
      },
      {
        request: { method: "GET", path: "/api/v1/settings/secrets" },
        // Exact: an extra field here would be a value leaking beside the name.
        //
        // Only secret-typed SETTINGS appear. The vault is one bag and also holds
        // `tls-key` (the sealed TLS private key) and D11's `mcp.<id>.*` token
        // bags, and these routes filter to the schema's `password` fields so
        // those are neither listed nor reachable. See D-027: without that filter
        // any paired client could delete `tls-key` and lock out every other one.
        expect: { status: 200, body: { names: ["llm.external.apiKey"] } },
      },
      {
        // The secret is not smuggled back through the settings view either.
        request: { method: "GET", path: SETTINGS },
        expect: { status: 200, bodyContains: { "llm.external.apiKey": "<absent>" } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/settings/secrets/llm.external.apiKey" },
        expect: { status: 204 },
      },
      {
        request: { method: "GET", path: "/api/v1/settings/secrets" },
        expect: { status: 200, body: { names: [] } },
      },
    ],
  },
  {
    id: "d1.secrets.deleting-one-that-was-never-set-is-a-404",
    package: "D1",
    routes: ["DELETE /api/v1/settings/secrets/:name"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: { method: "DELETE", path: "/api/v1/settings/secrets/never-set" },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
    ],
  },
  {
    id: "d1.secrets.rejects-a-body-without-a-string-value",
    package: "D1",
    routes: ["PUT /api/v1/settings/secrets/:name"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: {
          method: "PUT",
          path: "/api/v1/settings/secrets/llm.external.apiKey",
          body: { value: 42 },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    // Both stores are on-disk state, and this is the only end-to-end check that
    // the process which did not write them reads them back: settings.json for
    // the shared half, the client_settings table for the overlay.
    id: "d1.settings.both-stores-survive-a-restart",
    package: "D1",
    routes: ["PATCH /api/v1/settings", "GET /api/v1/settings"],
    auth: "paired",
    scenario: "d1",
    steps: [
      {
        request: {
          method: "PATCH",
          path: SETTINGS,
          body: { "llm.contextSize": 2048, "llm.topK": 33 },
        },
        expect: { status: 200, bodyContains: { "llm.contextSize": 2048, "llm.topK": 33 } },
      },
      {
        handoff: "restart",
        request: { method: "GET", path: SETTINGS },
        expect: {
          status: 200,
          pinUnchanged: true,
          bodyContains: {
            "llm.contextSize": 2048,
            "llm.topK": 33,
            "llm.provider": "external",
          },
        },
      },
    ],
  },
  {
    id: "d1.secrets.the-tls-key-is-not-reachable-through-settings",
    package: "D1",
    routes: [
      "GET /api/v1/settings/secrets",
      "PUT /api/v1/settings/secrets/:name",
      "DELETE /api/v1/settings/secrets/:name",
    ],
    auth: "paired",
    scenario: "d1",
    steps: [
      // The sealed TLS private key shares the vault with the settings secrets.
      // Deleting it makes the next boot generate a fresh keypair with a
      // different SPKI pin, which locks out every paired client at once; a bad
      // PUT means the core does not boot at all. Both are reachable by any
      // paired client, with no admin token, unless these routes filter to the
      // schema's secret-typed settings. See D-027.
      {
        request: { method: "DELETE", path: "/api/v1/settings/secrets/tls-key" },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: {
          method: "PUT",
          path: "/api/v1/settings/secrets/tls-key",
          body: { value: "not-a-jwk" },
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      // Nor an MCP token bag, which D11 will own and write through its own routes.
      {
        request: {
          method: "PUT",
          path: "/api/v1/settings/secrets/mcp.someserver.authToken",
          body: { value: "stolen" },
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: { method: "GET", path: "/api/v1/settings/secrets" },
        expect: { status: 200, body: { names: [] } },
      },
    ],
  },
];
