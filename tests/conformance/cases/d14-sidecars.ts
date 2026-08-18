// D14: sidecar and process supervision.
//
// One route, and its `sidecars` array is empty on BOTH targets: the harness
// home configures external providers everywhere, so the Deno oracle starts no
// sidecar either, and the Rust core has none to start. That is the point rather
// than a gap. What the route still carries is the core process's own pid and
// resource usage, which is the "Core Service" row in the Services settings
// field.
//
// `POST /api/v1/sidecars/:kind/{start,stop,restart}` has no case here on
// purpose. Its only two kinds were `llama` and `speech`, both of which become
// in-process engines, so the routes re-home to N2 and N3 along with the
// settings that decide how to start each one. The client's Retry button is the
// acceptance criterion for that move.

import type { Case } from "../runner/types.ts";

export const cases: Case[] = [
  {
    id: "d14.sidecars.status-needs-a-bearer",
    package: "D14",
    routes: ["GET /api/v1/sidecars/status"],
    auth: "none",
    steps: [
      {
        // Process ids and memory figures are not something an unauthenticated
        // caller gets to enumerate.
        request: { method: "GET", path: "/api/v1/sidecars/status" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "d14.sidecars.status-reports-the-core-process",
    package: "D14",
    routes: ["GET /api/v1/sidecars/status"],
    auth: "paired",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/sidecars/status" },
        expect: {
          status: 200,
          body: {
            sidecars: [],
            // The matcher set has no "> 0", so this asserts the shape and the
            // key set only. Whether the numbers are a real sample rather than
            // zeroes is asserted where it can be:
            // `sidecars::tests::the_status_route_reports_the_core_process_and_the_published_rows`
            // checks `rssMb` is positive, which a handler returning a
            // well-shaped placeholder would fail.
            core: { pid: "<number>", rssMb: "<number>", cpuPct: "<number>" },
          },
        },
      },
    ],
  },
];
