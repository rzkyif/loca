// D7: the binaries slice of the download engine.
//
// `/api/v1/binaries` had no case at all, which is how `GET /api/v1/binaries`
// came to reach the network on a shipping channel without anything noticing.
// The route is desktop-only (`runner/scope.ts` scopes it out for mobile,
// because a phone executes no downloaded binary), so on desktop it is a real
// comparison against the oracle rather than a skipped one.
//
// # What is asserted, and what deliberately is not
//
// The `deno` row is asserted exactly, because both cores agree on every field
// of it. The OTHER two kinds are not: this core installs only `deno` (D14
// absorbed the sidecars), so it offers no `desiredVariant` for `llama-server`
// or `loca-core-speech` where the oracle still does. That is the tracked
// difference recorded in the package's status, and asserting it here would
// either bake the difference into the contract or fail the oracle. `bodyContains`
// is what keeps this case honest about the distinction instead of hiding it.
//
// `/check` and `/probe` have no case yet ON PURPOSE. Both resolve an upstream
// GitHub release, and the harness only gained `LOCA_GITHUB_API_BASE_URL` on the
// Rust side; until the harness sets it for both targets a case here would send
// the oracle to the real api.github.com, which is exactly what
// `harness/upstream.ts:6-9` promises never happens. See the interfaces queue.

import type { Case } from "../runner/types.ts";

export const cases: Case[] = [
  {
    id: "d7.binaries.list-needs-a-bearer",
    package: "D7",
    routes: ["GET /api/v1/binaries"],
    auth: "none",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/binaries" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "d7.binaries.list-reports-the-pinned-worker-runtime",
    package: "D7",
    routes: ["GET /api/v1/binaries"],
    auth: "paired",
    steps: [
      {
        // `deno` is the one kind that still matters after the shrink: it is
        // what every extension tool worker runs in, so a core that cannot
        // report its state cannot offer to install it either.
        request: { method: "GET", path: "/api/v1/binaries" },
        expect: {
          status: 200,
          bodyContains: [
            // The two absorbed kinds, whose rows the two cores DO differ on:
            // this core offers no `desiredVariant` for a kind it cannot
            // install, and reports "unknown" where the oracle still resolves a
            // manifest entry. Held at `<any>` so the difference stays a
            // recorded one rather than something this case either bakes in or
            // fails the oracle over.
            "<any>",
            "<any>",
            {
              kind: "deno",
              // A resolver entry (dev and latest both resolve at runtime) does
              // not know its version without asking GitHub, and asking to
              // answer a plain list is what the consent rule forbids. So the
              // literal "latest" is the answer, not a version number.
              version: "latest",
              installed: false,
              desiredVariant: "cpu",
            },
          ],
        },
      },
    ],
  },
  {
    id: "d7.binaries.an-explicit-empty-kinds-list-installs-nothing",
    package: "D7",
    routes: ["POST /api/v1/binaries/install"],
    auth: "paired",
    steps: [
      {
        // D-028: absent means "everything missing", empty means "nothing". A
        // client that sends `[]` must not trigger a full install, and the
        // difference is invisible unless something asserts it.
        request: { method: "POST", path: "/api/v1/binaries/install", body: { kinds: [] } },
        expect: { status: 200, body: { jobIds: [] } },
      },
      {
        // And nothing was queued behind it, which is the half a jobIds check
        // cannot see: a core that started a download and forgot to return its
        // id would pass the step above.
        request: { method: "GET", path: "/api/v1/models/downloads" },
        expect: { status: 200, body: [] },
      },
    ],
  },
];
