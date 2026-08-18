// D11: MCP server administration.
//
// Thirteen routes: the three listings, CRUD, reconnect, the OAuth start, the
// per-tool and per-prompt toggles, the always-available opt-out, and the live
// prompt resolve.
//
// What these deliberately do NOT do, and why:
//
//   - **No case connects to anything.** Every server here is either disabled or
//     points at an address nothing answers on. There is no MCP server for the
//     harness to talk to (the loopback upstream speaks the OpenAI-compatible
//     surface, not MCP), and a real connect attempt would produce a status
//     message whose wording is implementation-specific, which reads as a
//     divergence in diff mode (D-029). Live connection behaviour is covered by
//     Rust tests against an in-process fake server, where a transport can be
//     staged directly.
//   - **No case completes an OAuth sign-in.** It needs a browser and an
//     authorization server. The refusal path (a non-remote server) is asserted
//     here; the rest is covered by unit tests.
//
// A note on `updatedAtMs`: creating a server with an `authToken` writes the row,
// then the vault, then records `hasAuth` on the row, so the second write moves
// `updatedAtMs` past `createdAtMs`. Both are asserted as `<ms>`, so the cases
// pass either way and diff mode stays quiet.

import type { Case } from "../runner/types.ts";

/** The three per-run values every projection carries.
 *
 * Asserted with placeholders rather than left out, because differential mode
 * compares any field a case does not mention RAW, and two cores never mint the
 * same ULID or the same millisecond (D-029). */
const STAMPS = {
  id: "<id>",
  createdAtMs: "<ms>",
  updatedAtMs: "<ms>",
};

/** The shape every projection has, with the per-run values as placeholders. */
const DISCONNECTED = {
  ...STAMPS,
  status: "disconnected",
  toolCount: 0,
  promptCount: 0,
  resourceCount: 0,
  toolEnabled: [],
  promptEnabled: [],
  toolAlwaysAvailableOff: [],
};

export const cases: Case[] = [
  {
    id: "d11.mcp.list-is-empty-on-a-fresh-core",
    package: "D11",
    routes: ["GET /api/v1/mcp"],
    auth: "paired",
    scenario: "d11-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/mcp" },
        expect: { status: 200, body: { servers: [] } },
      },
    ],
  },
  {
    id: "d11.mcp.prompt-and-resource-listings-are-empty-with-nothing-connected",
    package: "D11",
    routes: ["GET /api/v1/mcp/prompts", "GET /api/v1/mcp/resources"],
    auth: "paired",
    scenario: "d11-read",
    steps: [
      {
        // Also pins the routing: `/prompts` is a listing, not a server whose id
        // happens to be "prompts".
        request: { method: "GET", path: "/api/v1/mcp/prompts" },
        expect: { status: 200, body: { prompts: [] } },
      },
      {
        request: { method: "GET", path: "/api/v1/mcp/resources" },
        expect: { status: 200, body: { resources: [] } },
      },
    ],
  },
  {
    id: "d11.mcp.requires-a-paired-client",
    package: "D11",
    routes: ["GET /api/v1/mcp"],
    auth: "none",
    scenario: "d11-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/mcp" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "d11.mcp.create-projects-the-full-default-shape",
    package: "D11",
    routes: ["POST /api/v1/mcp", "GET /api/v1/mcp/{id}"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Weather", kind: "stdio", command: "npx", args: ["-y", "weather"] },
        },
        expect: {
          status: 201,
          body: {
            ...DISCONNECTED,
            name: "Weather",
            kind: "stdio",
            command: "npx",
            args: ["-y", "weather"],
            // Every default a client renders a control for: a new server is
            // disabled, runs its command verbatim, and grants the deno runtime
            // everything only if it is ever switched to it.
            runtime: "custom",
            denoAllowAll: true,
            denoPermissions: [],
            remoteAuth: "none",
            hasAuth: false,
            oauthAuthorized: false,
            enabled: false,
          },
        },
        capture: { serverId: "$.id" },
      },
      {
        request: { method: "GET", path: "/api/v1/mcp/${serverId}" },
        expect: {
          status: 200,
          bodyContains: { ...STAMPS, name: "Weather", status: "disconnected" },
        },
      },
    ],
  },
  {
    id: "d11.mcp.create-rejects-a-blank-name",
    package: "D11",
    routes: ["POST /api/v1/mcp"],
    auth: "paired",
    scenario: "d11-validation",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/mcp", body: { name: "   ", kind: "stdio" } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d11.mcp.create-rejects-an-enabled-server-with-no-transport-target",
    package: "D11",
    routes: ["POST /api/v1/mcp"],
    auth: "paired",
    scenario: "d11-validation",
    steps: [
      {
        // A disabled draft may be saved incomplete; enabling is the gate.
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "No command", kind: "stdio", enabled: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "No url", kind: "remote", enabled: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    // The patch body's `name` keeps its `.min(1)`, and the read-back is what
    // makes the case worth writing: the registry's update treats a blank name
    // as "keep the stored one", so a core that accepts `""` answers 200 with
    // the name unchanged, which is indistinguishable from a successful no-op
    // patch unless the status is asserted. The Rust core did exactly that while
    // the schema was route-local and the bridge could not see it. See D-093.
    id: "d11.mcp.patch-rejects-a-blank-name",
    package: "D11",
    routes: [
      "POST /api/v1/mcp",
      "PATCH /api/v1/mcp/:id",
      "GET /api/v1/mcp/:id",
      "DELETE /api/v1/mcp/:id",
    ],
    auth: "paired",
    scenario: "d11-validation",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Renamable", kind: "stdio" },
        },
        expect: { status: 201, bodyContains: { ...STAMPS, name: "Renamable" } },
        capture: { patchable: "$.id" },
      },
      {
        request: { method: "PATCH", path: "/api/v1/mcp/${patchable}", body: { name: "" } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "GET", path: "/api/v1/mcp/${patchable}" },
        expect: {
          status: 200,
          bodyContains: { ...STAMPS, id: "${patchable}", name: "Renamable" },
        },
      },
      {
        // Strict survives `.partial()`: an unknown key is refused on the patch
        // as well as on the create.
        request: {
          method: "PATCH",
          path: "/api/v1/mcp/${patchable}",
          body: { name: "Fine", notAField: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/mcp/${patchable}" },
        expect: { status: 200, body: { ok: true } },
      },
    ],
  },
  {
    id: "d11.mcp.create-rejects-an-unknown-field",
    package: "D11",
    routes: ["POST /api/v1/mcp"],
    auth: "paired",
    scenario: "d11-validation",
    steps: [
      {
        // The body is `.strict()` in `@loca/shared`, and D-031 settled that the
        // generator obeys what Zod declares, so this is a 400 on both cores.
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "X", kind: "stdio", notAField: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d11.mcp.get-and-patch-of-an-unknown-id-are-not-found",
    package: "D11",
    routes: ["GET /api/v1/mcp/{id}", "PATCH /api/v1/mcp/{id}"],
    auth: "paired",
    scenario: "d11-validation",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/mcp/01JZZZZZZZZZZZZZZZZZZZZZZZ" },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/mcp/01JZZZZZZZZZZZZZZZZZZZZZZZ",
          body: { name: "Renamed" },
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
    ],
  },
  {
    id: "d11.mcp.patch-renames-without-touching-the-connection-config",
    package: "D11",
    routes: ["PATCH /api/v1/mcp/{id}"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Before", kind: "stdio", command: "npx", args: ["-y", "server"] },
        },
        expect: { status: 201, bodyContains: STAMPS },
        capture: { serverId: "$.id" },
      },
      {
        request: {
          method: "PATCH",
          path: "/api/v1/mcp/${serverId}",
          body: { name: "After" },
        },
        expect: {
          status: 200,
          bodyContains: {
            ...STAMPS,
            name: "After",
            command: "npx",
            args: ["-y", "server"],
            enabled: false,
            status: "disconnected",
          },
        },
      },
    ],
  },
  {
    id: "d11.mcp.a-bearer-token-is-stored-and-never-echoed",
    package: "D11",
    routes: ["POST /api/v1/mcp", "PATCH /api/v1/mcp/{id}", "GET /api/v1/mcp/{id}"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        // `authToken` is write-only: the projection reports only `hasAuth`, and
        // the token itself goes to the secrets vault.
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: {
            name: "Remote",
            kind: "remote",
            url: "https://mcp.invalid/mcp",
            remoteAuth: "bearer",
            authToken: "conformance-token",
          },
        },
        expect: {
          status: 201,
          bodyContains: {
            ...STAMPS,
            hasAuth: true,
            remoteAuth: "bearer",
            url: "https://mcp.invalid/mcp",
          },
        },
        capture: { serverId: "$.id" },
      },
      {
        // An omitted token leaves the stored one alone.
        request: {
          method: "PATCH",
          path: "/api/v1/mcp/${serverId}",
          body: { name: "Renamed" },
        },
        expect: { status: 200, bodyContains: { ...STAMPS, hasAuth: true } },
      },
      {
        // An empty string clears it.
        request: {
          method: "PATCH",
          path: "/api/v1/mcp/${serverId}",
          body: { authToken: "" },
        },
        expect: { status: 200, bodyContains: { ...STAMPS, hasAuth: false } },
      },
    ],
  },
  {
    id: "d11.mcp.an-mcp-token-is-not-reachable-as-a-settings-secret",
    package: "D11",
    routes: ["POST /api/v1/mcp"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: {
            name: "Remote",
            kind: "remote",
            url: "https://mcp.invalid/mcp",
            remoteAuth: "bearer",
            authToken: "conformance-token",
          },
        },
        expect: { status: 201, bodyContains: { ...STAMPS, hasAuth: true } },
        capture: { serverId: "$.id" },
      },
      {
        // D-027: the vault is one bag, and these routes manage exactly one
        // class of secret. An MCP credential must not appear here, or any
        // paired client could delete another device's stored token.
        request: { method: "GET", path: "/api/v1/settings/secrets" },
        expect: { status: 200, body: { names: [] } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/settings/secrets/mcp.${serverId}.authToken" },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
    ],
  },
  {
    id: "d11.mcp.delete-removes-the-server",
    package: "D11",
    routes: ["DELETE /api/v1/mcp/{id}", "GET /api/v1/mcp"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Temporary", kind: "stdio", command: "npx" },
        },
        expect: { status: 201, bodyContains: STAMPS },
        capture: { serverId: "$.id" },
      },
      {
        request: { method: "DELETE", path: "/api/v1/mcp/${serverId}" },
        expect: { status: 200, body: { ok: true } },
      },
      {
        request: { method: "GET", path: "/api/v1/mcp" },
        expect: { status: 200, body: { servers: [] } },
      },
      {
        request: { method: "GET", path: "/api/v1/mcp/${serverId}" },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
    ],
  },
  {
    id: "d11.mcp.tool-toggles-round-trip",
    package: "D11",
    routes: ["POST /api/v1/mcp/{id}/tools/{tool}/{action}"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Toggles", kind: "stdio", command: "npx" },
        },
        expect: { status: 201, bodyContains: { ...STAMPS, toolEnabled: [] } },
        capture: { serverId: "$.id" },
      },
      {
        request: { method: "POST", path: "/api/v1/mcp/${serverId}/tools/search/enable" },
        expect: { status: 200, bodyContains: { ...STAMPS, toolEnabled: ["search"] } },
      },
      {
        // A set, so enabling twice is still one entry.
        request: { method: "POST", path: "/api/v1/mcp/${serverId}/tools/search/enable" },
        expect: { status: 200, bodyContains: { ...STAMPS, toolEnabled: ["search"] } },
      },
      {
        request: { method: "POST", path: "/api/v1/mcp/${serverId}/tools/search/disable" },
        expect: { status: 200, bodyContains: { ...STAMPS, toolEnabled: [] } },
      },
      {
        request: { method: "POST", path: "/api/v1/mcp/${serverId}/tools/search/toggle" },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d11.mcp.always-available-is-an-opt-out-set",
    package: "D11",
    routes: ["POST /api/v1/mcp/{id}/tools/{tool}/always-available/{action}"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Always", kind: "stdio", command: "npx" },
        },
        expect: { status: 201, bodyContains: { ...STAMPS, toolAlwaysAvailableOff: [] } },
        capture: { serverId: "$.id" },
      },
      {
        // MCP tools are always-available by default, so turning it OFF is what
        // adds a name. The set is the opt-out, not the opt-in.
        request: {
          method: "POST",
          path: "/api/v1/mcp/${serverId}/tools/search/always-available/disable",
        },
        expect: { status: 200, bodyContains: { ...STAMPS, toolAlwaysAvailableOff: ["search"] } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp/${serverId}/tools/search/always-available/enable",
        },
        expect: { status: 200, bodyContains: { ...STAMPS, toolAlwaysAvailableOff: [] } },
      },
    ],
  },
  {
    id: "d11.mcp.prompt-toggles-round-trip",
    package: "D11",
    routes: ["POST /api/v1/mcp/{id}/prompts/{prompt}/{action}"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Prompts", kind: "stdio", command: "npx" },
        },
        expect: { status: 201, bodyContains: { ...STAMPS, promptEnabled: [] } },
        capture: { serverId: "$.id" },
      },
      {
        request: { method: "POST", path: "/api/v1/mcp/${serverId}/prompts/commit/enable" },
        expect: { status: 200, bodyContains: { ...STAMPS, promptEnabled: ["commit"] } },
      },
      {
        request: { method: "POST", path: "/api/v1/mcp/${serverId}/prompts/commit/disable" },
        expect: { status: 200, bodyContains: { ...STAMPS, promptEnabled: [] } },
      },
    ],
  },
  {
    id: "d11.mcp.reconnect-on-a-disconnected-server-returns-the-projection",
    package: "D11",
    routes: ["POST /api/v1/mcp/{id}/reconnect"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Idle", kind: "stdio", command: "npx" },
        },
        expect: { status: 201, bodyContains: STAMPS },
        capture: { serverId: "$.id" },
      },
      {
        // The server is disabled, so this reconciles to "still nothing to
        // connect" and reaches no process and no network.
        request: { method: "POST", path: "/api/v1/mcp/${serverId}/reconnect" },
        expect: { status: 200, bodyContains: { ...STAMPS, name: "Idle", status: "disconnected" } },
      },
    ],
  },
  {
    id: "d11.mcp.oauth-start-is-refused-for-anything-but-a-remote-url",
    package: "D11",
    routes: ["POST /api/v1/mcp/{id}/oauth/start"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Local", kind: "stdio", command: "npx" },
        },
        expect: { status: 201, bodyContains: STAMPS },
        capture: { serverId: "$.id" },
      },
      {
        // Refused before anything is discovered, so no authorization server is
        // contacted and no loopback listener is opened.
        request: { method: "POST", path: "/api/v1/mcp/${serverId}/oauth/start" },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d11.mcp.resolving-a-prompt-validates-its-body-before-reaching-the-server",
    package: "D11",
    routes: ["POST /api/v1/mcp/{id}/prompts/{prompt}/resolve"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Offline", kind: "stdio", command: "npx" },
        },
        expect: { status: 201, bodyContains: STAMPS },
        capture: { serverId: "$.id" },
      },
      {
        // `.strict()`. The body was the TWELFTH route-local Zod schema, missed
        // by D-093's scan and now in `validation/mcp.ts`, so this strictness is
        // generated rather than restated.
        request: {
          method: "POST",
          path: "/api/v1/mcp/${serverId}/prompts/commit/resolve",
          body: { argumnets: {} },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // D-098. `arguments` is `.optional()`, so null is refused where absence
        // is not. The refusal has to happen BEFORE the server round-trip, which
        // is what distinguishes it from the 500 the next case asserts.
        request: {
          method: "POST",
          path: "/api/v1/mcp/${serverId}/prompts/commit/resolve",
          body: { arguments: null },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // Absence still reaches the server, which is how we know the refusals
        // above are about the value and not about the key.
        request: {
          method: "POST",
          path: "/api/v1/mcp/${serverId}/prompts/commit/resolve",
          body: {},
        },
        expect: { status: 500, bodyContains: { error: { code: "internal_error" } } },
      },
    ],
  },
  {
    id: "d11.mcp.resolving-a-prompt-on-a-disconnected-server-fails",
    package: "D11",
    routes: ["POST /api/v1/mcp/{id}/prompts/{prompt}/resolve"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Offline", kind: "stdio", command: "npx" },
        },
        expect: { status: 201, bodyContains: STAMPS },
        capture: { serverId: "$.id" },
      },
      {
        // Also the routing assertion that matters most: `resolve` must not be
        // read as an enable/disable action, which would answer 400 rather than
        // reaching the server.
        request: {
          method: "POST",
          path: "/api/v1/mcp/${serverId}/prompts/commit/resolve",
          body: { arguments: {} },
        },
        // 500 rather than 503: the Deno core raises a plain error for "not
        // connected", and the Rust core matches it rather than picking a
        // better code that would make this route unassertable while both cores
        // run. `Manager::session_or_err` records that, and the client shows one
        // notice for any failure here either way.
        expect: { status: 500, bodyContains: { error: { code: "internal_error" } } },
      },
    ],
  },
];
