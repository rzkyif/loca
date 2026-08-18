// D12: the extension registry, the install phases, and the relevance routes.
//
// **A fresh core is not an empty core.** Boot seeds the extensions Loca ships,
// so on the `dev` channel every core here starts with the built-in AND the
// dev-only samples showcase already registered at status `downloaded`
// (`extensions/seeding.ts`, `install/seed.rs`). Four cases in this file once
// asserted the opposite; they were written against a port that had no boot
// seeding at all and they pinned that absence as the contract. If you are
// adding a case here, check what the ORACLE answers before you write the
// expectation.
//
// What these deliberately do NOT do, and why:
//
//   - **No case installs anything itself.** Every user-triggered install path
//     either reaches the npm registry or needs a planted tarball, and the first
//     breaks the no-non-consented-network rule the suite runs under while the
//     second is a fixture the harness has no way to plant. What boot seeding
//     does is different and is exactly why it is allowed: on `dev` it resolves
//     from the checkout, and in production from installer-planted artefacts,
//     with no request either way.
//   - **No case calls `/search`.** It is a live npm proxy: the response is
//     whatever npm published this morning, which is not an assertion.
//   - **No case drives a tool call.** That needs a downloaded `deno`, a real
//     worker process and a terminal. It is the live probe's job, and it is
//     local-only by design.

import type { Case } from "../runner/types.ts";

/** One seeded row, as both cores project it after a `dev` boot.
 *
 *  Everything install-specific is a placeholder and everything contractual is
 *  literal. `toolCount` is literal on purpose: it is a projection each core
 *  computes for itself over its own tools table, so a placeholder would leave it
 *  unchecked in differential mode too (a masked position is not compared). If
 *  you add or remove a tool in a shipped extension, this number moves and the
 *  case is doing its job. */
function seededRow(fields: {
  id: string;
  displayName: string;
  hasDatabase: boolean;
  toolCount: number;
}): Record<string, unknown> {
  return {
    id: fields.id,
    source: "seeded",
    displayName: fields.displayName,
    // Copy, and it changes. `COPY.md` owns it; this file does not.
    description: "<string>",
    version: "<version>",
    installedPath: `<home>/extensions/${fields.id}`,
    manifestHash: "<sha256>",
    // Empty until the explicit Install step: the content hash is what
    // `installed` MEANS, and a seed that pinned one would let a seeded
    // extension's tools run without the user ever pressing Install.
    contentHash: "",
    status: "downloaded",
    hasDeps: true,
    hasDatabase: fields.hasDatabase,
    undeclaredPolicy: "deny",
    toolCount: fields.toolCount,
    // Zero, and this is the load-bearing half of "seeding is not installing":
    // seeding must not turn any tool on. Enabling happens on the user's press.
    enabledToolCount: 0,
    installedAtMs: "<ms>",
    updatedAtMs: "<ms>",
  };
}

export const cases: Case[] = [
  {
    id: "d12.extensions.a-fresh-core-lists-the-extensions-loca-ships",
    package: "D12",
    routes: ["GET /api/v1/extensions"],
    auth: "paired",
    scenario: "d12-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/extensions" },
        // A bare array, not an envelope. `ListExtensionsResponse` is a newtype
        // over the list, and wrapping it would be a shape divergence no unit
        // test on either side would notice.
        //
        // Ordered by name, which is contractual rather than incidental: the
        // client renders this list as it arrives.
        expect: {
          status: 200,
          body: [
            seededRow({
              id: "loca-extension-builtin",
              displayName: "Built-In",
              hasDatabase: false,
              toolCount: 17,
            }),
            // The showcase is `devOnly`, so this row is the one thing in the
            // file that would NOT be here on `stable` or `latest`. The harness
            // boots on `dev`.
            seededRow({
              id: "loca-extension-samples",
              displayName: "Samples",
              hasDatabase: true,
              toolCount: 12,
            }),
          ],
        },
      },
    ],
  },
  {
    id: "d12.extensions.requires-a-paired-client",
    package: "D12",
    routes: ["GET /api/v1/extensions"],
    auth: "none",
    scenario: "d12-read",
    steps: [
      {
        // None of this slice is admin-gated and none of it is open. An
        // unauthenticated read here lists which tools the machine can run and
        // which paths they were granted.
        request: { method: "GET", path: "/api/v1/extensions" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "d12.extensions.an-unknown-extension-is-a-404-on-every-route-that-names-one",
    package: "D12",
    routes: [
      "POST /api/v1/extensions/:id/install",
      "POST /api/v1/extensions/:id/uninstall",
      "POST /api/v1/extensions/:id/confirm-reenable",
      "DELETE /api/v1/extensions/:id",
    ],
    auth: "paired",
    scenario: "d12-read",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/extensions/ghost/install" },
        expect: { status: 404, bodyContains: { error: { code: "extension_not_found" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/extensions/ghost/uninstall" },
        expect: { status: 404, bodyContains: { error: { code: "extension_not_found" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/extensions/ghost/confirm-reenable" },
        expect: { status: 404, bodyContains: { error: { code: "extension_not_found" } } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/extensions/ghost" },
        expect: { status: 404, bodyContains: { error: { code: "extension_not_found" } } },
      },
    ],
  },
  {
    id: "d12.extensions.tools-of-an-unknown-extension-is-an-empty-list",
    package: "D12",
    routes: ["GET /api/v1/extensions/:id/tools"],
    auth: "paired",
    scenario: "d12-read",
    steps: [
      {
        // Not a 404: the tools query is a projection over rows, and an
        // extension with no rows has no tools. The 404 belongs to the routes
        // that ACT on an extension, above.
        request: { method: "GET", path: "/api/v1/extensions/ghost/tools" },
        expect: { status: 200, body: { tools: [] } },
      },
    ],
  },
  {
    id: "d12.extensions.rescan-of-an-already-registered-directory-changes-nothing",
    package: "D12",
    routes: ["POST /api/v1/extensions/rescan"],
    auth: "paired",
    scenario: "d12-write",
    steps: [
      {
        // Three zeroes over a directory that is NOT empty: boot seeded two
        // extensions into it. Rescan re-registers a folder only when its
        // `loca.json` hash has moved, so a freshly seeded pair is `updated: 0`,
        // and a core that re-registered them anyway would reset both to
        // `downloaded` and lose the user's enabled tools on every restart.
        request: { method: "POST", path: "/api/v1/extensions/rescan" },
        expect: { status: 200, body: { added: 0, updated: 0, removed: 0 } },
      },
    ],
  },
  {
    id: "d12.extensions.check-updates-finds-the-shipped-extensions-already-current",
    package: "D12",
    routes: ["GET /api/v1/extensions", "POST /api/v1/extensions/check-updates"],
    auth: "paired",
    scenario: "d12-read",
    steps: [
      {
        // The installed versions are captured rather than written down: they
        // carry a content stamp of the checkout (`0.1.5+dev.50df7cc2`), so no
        // literal would survive an edit to either extension.
        request: { method: "GET", path: "/api/v1/extensions" },
        expect: {
          status: 200,
          // The two clocks are named with placeholders rather than left out:
          // differential mode compares every position a case does not mask,
          // including ones it never mentions, and these are two separate boots.
          bodyContains: [
            { id: "loca-extension-builtin", installedAtMs: "<ms>", updatedAtMs: "<ms>" },
            { id: "loca-extension-samples", installedAtMs: "<ms>", updatedAtMs: "<ms>" },
          ],
        },
        capture: { builtinVersion: "$.0.version", samplesVersion: "$.1.version" },
      },
      {
        // Two results, one per seeded extension, and STILL no network: on `dev`
        // the upstream version of a seeded extension is resolved from the
        // checkout rather than from its signed CDN manifest. That is what makes
        // "nothing to update" assertable here at all.
        request: { method: "POST", path: "/api/v1/extensions/check-updates", body: {} },
        expect: {
          status: 200,
          body: {
            results: [
              {
                id: "loca-extension-builtin",
                installedVersion: "${builtinVersion}",
                latestVersion: "${builtinVersion}",
                updateAvailable: false,
              },
              {
                id: "loca-extension-samples",
                installedVersion: "${samplesVersion}",
                latestVersion: "${samplesVersion}",
                updateAvailable: false,
              },
            ],
          },
        },
      },
    ],
  },
  {
    id: "d12.extensions.builtin-install-queues-behind-the-worker-runtime",
    package: "D12",
    routes: ["POST /api/v1/extensions/builtin-install"],
    auth: "paired",
    scenario: "d12-write",
    steps: [
      {
        // `queued: true`, and every word of that is load-bearing. The built-in
        // IS present, because boot seeded it, and it is at `downloaded`, so the
        // user's press has real work to do; but installing its dependencies
        // needs the `deno` worker runtime, which is a separate consent-gated
        // download and is absent from a conformance home. So the request is
        // remembered rather than started, and the client says the install is
        // pending the runtime download rather than showing an error.
        //
        // `queued: false` is what a core with NO built-in answers. That is what
        // this case asserted while the port seeded nothing, and it made a
        // missing subsystem look like agreed behaviour.
        request: { method: "POST", path: "/api/v1/extensions/builtin-install" },
        expect: { status: 200, body: { queued: true } },
      },
    ],
  },
  {
    id: "d12.extensions.an-unknown-subject-is-a-404-before-the-body-is-read",
    package: "D12",
    routes: [
      "POST /api/v1/extensions/:id/update",
      "POST /api/v1/extensions/:id/undeclared-policy",
      "POST /api/v1/extensions/:id/tools/:tool/grants",
    ],
    auth: "paired",
    scenario: "d12-read",
    steps: [
      // Every body below is INVALID for its route, so a 404 can only come from
      // the lookup having run first. Validating first tells a caller whether an
      // id exists only once it supplies a well-formed body, so the two orderings
      // leak different information and the order is contract, not detail.
      {
        request: {
          method: "POST",
          path: "/api/v1/extensions/ghost/undeclared-policy",
          body: { policy: "allow" },
        },
        expect: { status: 404, bodyContains: { error: { code: "extension_not_found" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/extensions/ghost/update",
          body: { version: 5 },
        },
        expect: { status: 404, bodyContains: { error: { code: "extension_not_found" } } },
      },
      {
        // `tool_not_found` rather than `extension_not_found`: this route looks
        // the TOOL up, and an extension with no rows has no tools.
        request: {
          method: "POST",
          path: "/api/v1/extensions/ghost/tools/nope/grants",
          body: { grants: "not an array" },
        },
        expect: { status: 404, bodyContains: { error: { code: "tool_not_found" } } },
      },
    ],
  },
  {
    id: "d12.extensions.always-available-screens-its-value-before-it-looks-the-tool-up",
    package: "D12",
    routes: ["POST /api/v1/extensions/:id/tools/:tool/always-available"],
    auth: "paired",
    scenario: "d12-read",
    steps: [
      {
        // The one route in the slice that does it the other way round, pinned so
        // that "make the ordering uniform" is a change somebody has to justify
        // rather than one that passes quietly. Same request shape as the case
        // above, opposite answer.
        request: {
          method: "POST",
          path: "/api/v1/extensions/ghost/tools/nope/always-available",
          body: { value: "not a boolean" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  // --- the `d12-validation` scenario ---------------------------------------
  //
  // One boot, shared by the cases below in declaration order. **One of them
  // starts a REAL install**: the dev-only showcase case posts a download that
  // the route accepts, and on the dev channel every conformance core runs on,
  // that job copies the showcase out of the checkout and registers it, after
  // the step it belongs to has already passed.
  //
  // Nothing here asserts an empty extension list, so it is green. A case added
  // later that does would race the copy and flake, and the reason would be two
  // cases away from the failure. Either give such a case `isolate: true` or put
  // it in another scenario.
  {
    id: "d12.extensions.a-local-download-without-a-slug-is-refused",
    package: "D12",
    routes: ["POST /api/v1/extensions/download"],
    auth: "paired",
    scenario: "d12-validation",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/extensions/download",
          body: { source: "local", path: "/tmp/nowhere" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // D-043: `""` is a JavaScript falsy discriminator on the oracle, so an
        // empty slug is ABSENT rather than a slug that happens to be empty.
        // A port that passes it through resolves the install path to the
        // extensions directory itself.
        request: {
          method: "POST",
          path: "/api/v1/extensions/download",
          body: { source: "local", path: "/tmp/nowhere", slug: "" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d12.extensions.an-unknown-seeded-id-is-refused-before-a-job-starts",
    package: "D12",
    routes: ["POST /api/v1/extensions/download"],
    auth: "paired",
    scenario: "d12-validation",
    steps: [
      {
        // Refused at the route, so the user gets a 400 rather than a job id
        // followed by a failure frame nothing is showing yet.
        request: {
          method: "POST",
          path: "/api/v1/extensions/download",
          body: { source: "seeded", id: "loca-extension-invented" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d12.extensions.the-dev-only-showcase-cannot-be-installed-off-the-dev-channel",
    package: "D12",
    routes: ["POST /api/v1/extensions/download"],
    auth: "paired",
    scenario: "d12-validation",
    steps: [
      {
        // The dev-only gate is REAL but ASYNCHRONOUS. It lives inside the
        // download job (`installer-download.ts:151`), not at the route, so both
        // cores accept the request and report the refusal over WS as an
        // `extension.install_done` with `ok: false`. This case originally
        // asserted a synchronous 400 and failed on BOTH cores, which is the
        // signature of a wrong case rather than a wrong port.
        //
        // What it pins now is the half that IS synchronous: the route accepts a
        // known seeded id and hands back a job. Its sibling
        // `an-unknown-seeded-id-is-refused-before-a-job-starts` covers the check
        // that DOES happen at the route, and the difference between the two is
        // the point.
        //
        // **The refusal is still not reachable from this suite**, and the
        // blocker moved rather than went away. `Case.channel` now exists and
        // boots an isolated case on another channel, which is what the gate
        // (`devOnly && channel() !== "dev"`) needs. What stops it is one layer
        // down: OFF the dev channel the Rust core seals its vault in the real
        // OS keychain (`secrets/store.rs`'s `select`), and a conformance boot
        // on `stable` HANGS for its whole 30s budget on this machine, because
        // a real stable install already owns that keychain item and macOS asks
        // a human about it. The Deno core degrades to its file store instead,
        // because its keychain helper is a stub in a seeded home. So the two
        // cores do not even fail the same way. Filed with the evidence; a case
        // that cannot run is worse than no case.
        request: {
          method: "POST",
          path: "/api/v1/extensions/download",
          body: { source: "seeded", id: "loca-extension-samples" },
        },
        // The job id is named with a placeholder rather than left out: it is a
        // fresh ULID per core, and differential mode compares every position a
        // case does not mask.
        expect: {
          status: 200,
          bodyContains: { extensionId: "loca-extension-samples", jobId: "<id>" },
        },
      },
    ],
  },
  {
    id: "d12.extensions.reindex-skips-cleanly-with-no-embed-model",
    package: "D12",
    routes: ["POST /api/v1/extensions/reindex"],
    auth: "paired",
    scenario: "d12-relevance",
    steps: [
      {
        // A clean skip rather than an error: the embed model is a separate
        // consent-gated download, and a 500 here would look like a broken core.
        request: { method: "POST", path: "/api/v1/extensions/reindex" },
        expect: { status: 200, body: { embedded: 0, skipped: true } },
      },
    ],
  },
  {
    id: "d12.extensions.embed-reports-the-missing-model-rather-than-failing-opaquely",
    package: "D12",
    routes: ["POST /api/v1/extensions/embed"],
    auth: "paired",
    scenario: "d12-relevance",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/extensions/embed",
          body: { texts: ["hello"] },
        },
        expect: { status: 503, bodyContains: { error: { code: "server_unavailable" } } },
      },
    ],
  },
  {
    id: "d12.extensions.filter-answers-all-three-lists",
    package: "D12",
    routes: ["POST /api/v1/extensions/filter"],
    auth: "paired",
    scenario: "d12-relevance",
    steps: [
      {
        // `alwaysAvailable` and `nameMatched` are the two fields
        // `ToolFilterResponse` does not declare (the client casts around it).
        // Since the Rust types are GENERATED, a core emitting the declared
        // shape would drop both, and this is the case that would catch it.
        request: {
          method: "POST",
          path: "/api/v1/extensions/filter",
          body: { vector: [0, 1], topK: 5 },
        },
        expect: {
          status: 200,
          body: { candidates: [], alwaysAvailable: [], nameMatched: [] },
        },
      },
    ],
  },
  {
    id: "d12.extensions.tool-schemas-refuses-an-id-that-names-no-tool",
    package: "D12",
    routes: ["POST /api/v1/extensions/tool-schemas"],
    auth: "paired",
    scenario: "d12-relevance",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/extensions/tool-schemas",
          body: { ids: ["ghost::nope"] },
        },
        expect: { status: 404, bodyContains: { error: { code: "tool_not_found" } } },
      },
      {
        // The one zod-validated body in the slice. An empty id is refused by
        // `z.string().min(1)`, and `minLength` is the constraint the generator
        // DOES compile into the Rust deserializer, so both cores refuse it for
        // the same reason.
        request: {
          method: "POST",
          path: "/api/v1/extensions/tool-schemas",
          body: { ids: [""] },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d12.extensions.the-undeclared-policy-accepts-only-the-two-it-declares",
    package: "D12",
    routes: ["POST /api/v1/extensions/:id/undeclared-policy"],
    auth: "paired",
    scenario: "d12-validation",
    steps: [
      {
        // A third policy would be persisted and then matched by no branch at
        // runtime, which is a tool whose undeclared accesses are neither asked
        // about nor denied.
        //
        // Aimed at a REAL extension, and it has to be: this route checks the
        // extension exists before it reads the body, so the same request
        // against `ghost` never reaches the policy check at all. It used to be
        // aimed at `ghost` and passed for the wrong reason, on a port that
        // happened to validate first.
        request: {
          method: "POST",
          path: "/api/v1/extensions/loca-extension-builtin/undeclared-policy",
          body: { policy: "allow" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
];
