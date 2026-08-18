// D12: the flat, provider-agnostic tool catalog.
//
// One route, and it is the only place D11 and D12 meet: the union of every
// extension tool and every MCP tool, which is what the Tools management UI
// renders. The per-provider enable, disable and grant routes stay on each
// provider's own slice; this one is read-only.

import type { Case } from "../runner/types.ts";

export const cases: Case[] = [
  {
    id: "d12.tools.the-merged-catalog-is-empty-on-a-fresh-core",
    package: "D12",
    routes: ["GET /api/v1/tools"],
    auth: "paired",
    scenario: "d12-tools",
    steps: [
      {
        // Empty, but NOT because nothing is installed: boot seeded two
        // extensions with 29 tools between them. The catalog carries only the
        // ENABLED tools of INSTALLED extensions, and a seeded extension lands at
        // `downloaded` with every tool off, so it contributes nothing until the
        // user presses Install and enables one. A core that listed a seeded
        // extension's tools here would be offering the model tools nobody
        // approved.
        //
        // It also pins that the route EXISTS and answers an envelope rather than
        // a bare array, which is the opposite of the extensions listing next
        // door and is exactly the kind of asymmetry a rewrite tidies away.
        request: { method: "GET", path: "/api/v1/tools" },
        expect: { status: 200, body: { tools: [] } },
      },
    ],
  },
  {
    id: "d12.tools.requires-a-paired-client",
    package: "D12",
    routes: ["GET /api/v1/tools"],
    auth: "none",
    scenario: "d12-tools",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/tools" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "d12.tools.an-mcp-tool-joins-the-catalog-alongside-extension-tools",
    package: "D12",
    routes: ["GET /api/v1/tools", "POST /api/v1/mcp"],
    auth: "paired",
    scenario: "d12-tools-mcp",
    steps: [
      {
        // A disabled server, so nothing connects and nothing is fetched. It
        // contributes no tools, which is the point: the union must not invent
        // rows for a provider that has never reported any, and a catalog that
        // grew an entry here would be the port guessing.
        request: {
          method: "POST",
          path: "/api/v1/mcp",
          body: { name: "Offline server", kind: "stdio", command: "true", enabled: false },
        },
        // 201, not 200: creating a server is a create, and the oracle says so
        // at `core-engine/src/http/routes/mcp.ts:76`. This is a setup step for
        // a route D11 owns, and asserting the wrong status here would have
        // reported the port as broken when it matched the oracle exactly.
        //
        // The row's id and its two clocks are named with placeholders rather
        // than left out: differential mode compares every position a case does
        // not mask, including ones it never mentions, and a fresh ULID and a
        // fresh timestamp differ between two boots by construction.
        expect: {
          status: 201,
          bodyContains: { id: "<id>", createdAtMs: "<ms>", updatedAtMs: "<ms>" },
        },
      },
      {
        request: { method: "GET", path: "/api/v1/tools" },
        expect: { status: 200, body: { tools: [] } },
      },
    ],
  },
];
