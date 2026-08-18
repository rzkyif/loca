// D9: the storage browser.
//
// Three routes: the tree, the per-path reclaim, and the per-category clear.
//
// What these deliberately do NOT assert, and why:
//
//   - The `binaries` category's rows are target-dependent by construction. The
//     harness seeds four helper stubs for the Deno core and one for the Rust
//     core (harness/home.ts), and D7 shrinks BINARY_KINDS further. So the
//     category envelope is asserted here and its rows belong to D7.
//   - No case creates a session, an extension or a model, because each needs
//     another package's route or a download. The lock rules those would
//     exercise are covered by unit tests next to the code, where a lock can be
//     staged directly.
//   - No case runs the `settings` clear. It is a factory reset: destructive to
//     its whole scenario, and its result is the absence of state rather than
//     anything these three routes report.

import type { Case } from "../runner/types.ts";

/** The live `core.log`, which both cores open at boot and never rotate during a
 *  run.
 *
 *  Its SIZE is the one genuinely unstable number in this tree, and it is not
 *  only unstable across the two cores (they log different lines): it is
 *  unstable against itself, because the file grows between two requests inside
 *  a single shared scenario. Measured at 1597 bytes on the first case of the
 *  `d9-read` boot and 1709 on the next. Everything else about the row is
 *  fixed. */
const LOG_NODES = [
  { kind: "file", name: "core.log", path: "<home>/logs/core.log", size: "<number>" },
];

/** The three directories `ensureDirs` creates at boot: always present, always
 *  in this order, and the one category whose rows are fully determined by the
 *  layout rather than by what else ran. */
const CACHE_NODES = [
  { kind: "folder", name: "cache", path: "<home>/cache", size: "<number>" },
  { kind: "folder", name: "deno-cache", path: "<home>/deno-cache", size: "<number>" },
  { kind: "folder", name: "staging", path: "<home>/staging", size: "<number>" },
];

/** The seven categories, in the order the client draws its groups. Their rows
 *  vary by target and by what else ran; their identity, labels and order do
 *  not.
 *
 *  Two entries carry a deliberate loosening and neither is cosmetic:
 *
 *  - `extensions.nodes` is a TRACKED GAP, not noise. The Deno core installs the
 *    built-in extension on first boot and codebase-installs the samples bundle
 *    in the dev environment, so it reports two rows; the Rust core reports none,
 *    because D9's `ExtensionsSource` seam is still `unwired()` and D12 (the
 *    extension supervisor) has not been started. `<any>` records the gap
 *    instead of silencing it (D-029): when D12 lands and calls
 *    `.with_extensions(...)`, the two rows come back and this goes back to
 *    being an ordinary assertion. `size` follows the same rows, so it is
 *    loosened for the same reason.
 *  - `logs`, for the reason on `LOG_NODES` above.
 *
 *  What is deliberately NOT loosened is `settings.nodes[0].size`. The two cores
 *  report 761 and 743 bytes for it, and that 18-byte gap is a real divergence
 *  rather than a per-run value. The row is `settings.json` plus `secrets.enc`
 *  (services/storage.ts:389); both `settings.json` files are the same 469 bytes,
 *  differing only in the mock upstream port the harness gave each core. The gap
 *  is entirely in the sealed vault, whose plaintext holds the TLS private key as
 *  a JWK: WebCrypto's `exportKey("jwk")` writes `"alg":"ES256"` and the Rust
 *  `jwk_from_secret_key` (transport/tls.rs) does not, which is exactly 18 bytes
 *  once escaped inside the vault's JSON string. Masking it with `<number>`
 *  would hide the one thing in this tree still worth reporting, so it stays
 *  exposed until the port writes the same JWK. */
const CATEGORY_ENVELOPE = [
  { id: "models", label: "Models", deletable: true, size: "<number>" },
  { id: "binaries", label: "Binaries", deletable: true, size: "<number>" },
  { id: "cache", label: "Cache", deletable: true, size: "<number>" },
  { id: "logs", label: "Logs", deletable: true, size: "<number>", nodes: LOG_NODES },
  { id: "sessions", label: "Sessions", deletable: true, size: "<number>" },
  { id: "extensions", label: "Extensions", deletable: true, size: "<number>", nodes: "<any>" },
  { id: "settings", label: "Settings", deletable: true, size: "<number>" },
];

/** The envelope with some categories tightened, for a case that is about those
 *  rows. Everything not named keeps the envelope's assertion. */
function withCategories(over: Record<string, Record<string, unknown>>) {
  return CATEGORY_ENVELOPE.map((c) => ({ ...c, ...over[c.id] }));
}

/** Masks the settings row's size and nothing else, for the cases that read the
 *  tree without being about it. The divergence it covers is left exposed by
 *  `d9.storage.tree`, so a run reports it once instead of four times. */
const SETTINGS_SIZE_MASKED = [{ size: "<number>" }];

export const cases: Case[] = [
  {
    id: "d9.storage.tree",
    package: "D9",
    routes: ["GET /api/v1/storage"],
    auth: "paired",
    scenario: "d9-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/storage" },
        expect: {
          status: 200,
          bodyContains: {
            // The parent of the models directory, which is the core home only
            // because the harness sets LOCA_CORE_HOME. On a default install
            // models are shared across channels and this is ~/.loca.
            root_path: "<home>",
            // Sums every category, so it inherits both the log file's growth
            // and the absent extension rows.
            total_size: "<number>",
            categories: CATEGORY_ENVELOPE,
          },
        },
      },
    ],
  },
  {
    id: "d9.storage.tree.requires-a-paired-client",
    package: "D9",
    routes: ["GET /api/v1/storage"],
    auth: "none",
    scenario: "d9-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/storage" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "d9.storage.cache-rows",
    package: "D9",
    routes: ["GET /api/v1/storage"],
    auth: "paired",
    scenario: "d9-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/storage" },
        expect: {
          status: 200,
          bodyContains: {
            total_size: "<number>",
            categories: withCategories({
              cache: { nodes: CACHE_NODES },
              settings: { nodes: SETTINGS_SIZE_MASKED },
            }),
          },
        },
      },
    ],
  },
  {
    id: "d9.storage.settings-row-is-never-locked",
    package: "D9",
    routes: ["GET /api/v1/storage"],
    auth: "paired",
    scenario: "d9-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/storage" },
        expect: {
          status: 200,
          bodyContains: {
            total_size: "<number>",
            categories: withCategories({
              settings: {
                nodes: [
                  {
                    kind: "file",
                    name: "Settings + secrets",
                    path: "<home>/settings.json",
                    // This case is about the lock, so the size is masked here
                    // and left exposed by `d9.storage.tree`.
                    size: "<number>",
                    // Clearing it is a factory reset rather than a file
                    // removal, so it carries no lock even though the file is
                    // very much in use.
                    lock_reason: "<absent>",
                  },
                ],
              },
            }),
          },
        },
      },
    ],
  },
  {
    id: "d9.storage.delete.unknown-path-is-skipped",
    package: "D9",
    routes: ["POST /api/v1/storage/delete"],
    auth: "paired",
    scenario: "d9-read",
    steps: [
      {
        // A path that matches no row is a no-op, not an error. That is what
        // makes a repeated delete idempotent when the first one succeeded.
        request: {
          method: "POST",
          path: "/api/v1/storage/delete",
          body: { paths: ["/nowhere/at/all.gguf"] },
        },
        expect: { status: 204 },
      },
    ],
  },
  {
    id: "d9.storage.delete.traversal-is-skipped",
    package: "D9",
    routes: ["POST /api/v1/storage/delete", "GET /api/v1/storage"],
    auth: "paired",
    scenario: "d9-read",
    steps: [
      {
        // A caller's string is only ever a lookup key into the tree the core
        // just built, so an escape attempt matches nothing and removes
        // nothing. It is refused by not being found, not by a filter.
        request: {
          method: "POST",
          path: "/api/v1/storage/delete",
          body: {
            paths: [
              "<home>/../../../../etc/passwd",
              "/etc/passwd",
              "../../../../etc/passwd",
              // A real row spelled with a trailing separator is a different
              // key, so it matches nothing either.
              "<home>/cache/",
            ],
          },
        },
        expect: { status: 204 },
      },
      {
        // And the tree still holds the three cache rows, so nothing under the
        // core home was reclaimed on the way past.
        request: { method: "GET", path: "/api/v1/storage" },
        expect: {
          status: 200,
          bodyContains: {
            total_size: "<number>",
            categories: withCategories({
              cache: { nodes: [{ name: "cache" }, { name: "deno-cache" }, { name: "staging" }] },
              settings: { nodes: SETTINGS_SIZE_MASKED },
            }),
          },
        },
      },
    ],
  },
  {
    id: "d9.storage.delete.rejects-a-body-without-paths",
    package: "D9",
    routes: ["POST /api/v1/storage/delete"],
    auth: "paired",
    scenario: "d9-read",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/storage/delete", body: {} },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d9.storage.clear.rejects-an-unknown-category",
    package: "D9",
    routes: ["POST /api/v1/storage/clear"],
    auth: "paired",
    scenario: "d9-read",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/storage/clear",
          body: { categoryId: "not-a-category" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/storage/clear", body: {} },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d9.storage.clear.cache-empties-and-recreates",
    package: "D9",
    routes: ["POST /api/v1/storage/clear", "GET /api/v1/storage"],
    auth: "paired",
    // Empties directories the rest of the core writes to, so it pays for its
    // own boot rather than pulling them out from under a sibling case.
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/storage/clear",
          body: { categoryId: "cache" },
        },
        expect: { status: 204 },
      },
      {
        // The rows are still there, which is the assertion worth making: the
        // directories are recreated empty rather than removed, because the
        // rest of the core expects them for the whole run.
        request: { method: "GET", path: "/api/v1/storage" },
        expect: {
          status: 200,
          bodyContains: {
            total_size: "<number>",
            categories: withCategories({
              cache: {
                size: 0,
                nodes: [
                  { name: "cache", size: 0 },
                  { name: "deno-cache", size: 0 },
                  { name: "staging", size: 0 },
                ],
              },
              settings: { nodes: SETTINGS_SIZE_MASKED },
            }),
          },
        },
      },
    ],
  },
];
