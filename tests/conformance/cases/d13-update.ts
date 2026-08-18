// D13: self-update.
//
// Two routes, `GET /api/v1/update/check` and `POST /api/v1/update/apply`, both
// behind a bearer. Desktop-only: `runner/scope.ts` drops `/api/v1/update` for
// the mobile target because updates ship through the app store there, so on
// desktop these are real comparisons against the oracle rather than skips.
//
// # The harness serves no manifest, and that is the point rather than a limit
//
// The suite runs on `LOCA_CHANNEL=dev` with `LOCA_STORAGE_BASE_URL` pointed at
// the mock upstream (harness/core.ts), and the mock serves no `/manifests/`
// path at all. So `<upstream>/manifests/dev/core.json` is a 404 on BOTH cores.
// That makes these cases the proof that both refuse identically when the trust
// root cannot be reached, which is the failure a user behind a captive portal
// actually hits.
//
// # Three of these four pass because the operation could not START
//
// That is indistinguishable from passing because the operation behaved
// correctly, and it is the same shape as a `calls() == 0` assertion whose
// counter never counts. Two consequences, both deliberate:
//
//   - Every case asserts the EXACT error code, never merely a non-2xx.
//     `manifest_fetch_failed` and `signature_invalid` are both failures and
//     only one of them means what these cases claim: the first says the trust
//     root was unreachable, the second says it answered with something forged.
//     A core that reported the second here would be telling a user with a
//     captive portal that their release is compromised.
//   - `d13.update.apply-refuses-before-touching-anything` carries a SECOND
//     step, and that step is load-bearing rather than defensive. "The manifest
//     404ed" and "the core swapped its binary and exited" both produce a failed
//     request; only a subsequent successful request on the same core separates
//     them. **Deleting that step makes the case vacuous.**
//
// # What is NOT conformance-testable here, stated rather than left absent
//
//   - **A successful apply.** It ends by exiting the process, so there is no
//     shape of case that survives it and the harness would report the core as
//     crashed. Covered by Rust route tests against a recording exiter
//     (`update/routes.rs`), which is the only place the four supervisor exit
//     codes are observable at all.
//   - **Boot-time rollback, the commit, and the helper check.** All three run
//     before the listener binds, so nothing on the wire can observe them.
//     Covered by Rust tests over a tempdir.
//   - **Every install verb.** They are CLI subcommands, not routes.
//   - **A signed manifest end to end.** The harness has no signing key, so it
//     cannot serve a document the production trust root accepts. Filed as
//     informational for F8 (Q9), not as a blocker: a `signature_invalid` case
//     would need the mock upstream to serve a manifest path, which is F8's
//     file.
//
// # If these four 404 on `--core=rust`, the mount has regressed
//
// They pass on both cores today. `transport/router.rs` is orchestrator-owned,
// and `update::routes::routes` is mounted there with the ONE
// `CoreStatusService` boot builds, because the slice WRITES to it
// (`note_update_staged`). A 404 here means that merge line is gone, not that
// the port is incomplete: the routes themselves are covered by route-level
// Rust tests that assemble the shipping slice.
//
// Per D-029, every per-run value is written with a placeholder rather than
// omitted, so diff mode reports a real divergence instead of noise.

import type { Case } from "../runner/types.ts";
import { UNSTABLE_CAPABILITIES } from "./foundation/capabilities.ts";

export const cases: Case[] = [
  {
    id: "d13.update.check-needs-a-bearer",
    package: "D13",
    routes: ["GET /api/v1/update/check"],
    auth: "none",
    scenario: "d13-update",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/update/check" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "d13.update.apply-needs-a-bearer",
    package: "D13",
    routes: ["POST /api/v1/update/apply"],
    auth: "none",
    scenario: "d13-update",
    steps: [
      {
        // Unauthenticated, so this must be refused BEFORE the body is even
        // looked at. A core that parsed first and rejected after would leak
        // the fact that the route exists in a different shape.
        request: { method: "POST", path: "/api/v1/update/apply", body: {} },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "d13.update.check-refuses-an-unreachable-manifest",
    package: "D13",
    routes: ["GET /api/v1/update/check"],
    auth: "paired",
    scenario: "d13-update",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/update/check" },
        expect: {
          status: 502,
          bodyContains: {
            // Exact, not "some failure". See the file header: the two failure
            // codes mean opposite things to the user.
            error: { code: "manifest_fetch_failed", message: "<string>" },
          },
        },
      },
    ],
  },
  {
    id: "d13.update.apply-refuses-before-touching-anything",
    package: "D13",
    routes: ["POST /api/v1/update/apply", "GET /api/v1/health"],
    auth: "paired",
    scenario: "d13-update",
    steps: [
      {
        // No body at all, which is the normal shape: the client's
        // `update.apply()` with no argument serialises to `{}` and some send
        // nothing. Both cores must accept it rather than 400 it.
        request: { method: "POST", path: "/api/v1/update/apply" },
        expect: {
          status: 502,
          bodyContains: { error: { code: "manifest_fetch_failed", message: "<string>" } },
        },
      },
      {
        // THE load-bearing step. Without it this case cannot tell a refusal
        // apart from a core that swapped its binary and died: both look like a
        // failed request. A core still answering here did not reach the swap
        // and did not exit.
        request: { method: "GET", path: "/api/v1/health" },
        expect: {
          status: 200,
          bodyContains: {
            status: "<string>",
            version: "<string>",
            capabilities: UNSTABLE_CAPABILITIES,
          },
        },
      },
    ],
  },
];
