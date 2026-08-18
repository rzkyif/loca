// The case format. This file is the whole vocabulary a case author needs.
//
// Cases are DATA, not test code: object literals in a per-package file under
// cases/. There is no describe, no beforeEach, no assertion library. A package
// adds coverage by copying a neighbouring case and changing fields, which is
// the bar the format has to clear, because each package owns its own case file
// and nobody should have to read the runner to write one.

import type { LlmScript } from "../harness/upstream.ts";
import type { PlantedFile } from "../harness/home.ts";

export type { LlmScript, ScriptedToolCall } from "../harness/upstream.ts";
export type { PlantedFile } from "../harness/home.ts";

export type Method = "GET" | "POST" | "PATCH" | "PUT" | "DELETE" | "HEAD";

export type Json = unknown;

/** Which identity the request is made as.
 *  - none         no credentials at all
 *  - admin        X-Admin-Token, the installer's device-access path
 *  - paired       the scenario's primary paired client (the common case)
 *  - paired:other a SECOND paired client, so isolation cases have someone to
 *                 be isolated from */
export type Auth = "none" | "admin" | "paired" | "paired:other";

export type PackageId =
  | "F6"
  | "F7"
  | "D1"
  | "D2"
  | "D3"
  | "D4"
  | "D5"
  | "D6"
  | "D7"
  | "D8"
  | "D9"
  | "D10"
  | "D11"
  | "D12"
  | "D13"
  | "D14"
  | "D15";

/** One part of a multipart/form-data body. Either a plain field (`value`) or a
 *  file (`file`). Text values carry `${captured}` substitution like everything
 *  else. */
export interface FormPart {
  name: string;
  value?: string;
  file?: {
    filename: string;
    contentType?: string;
    /** Literal text, or base64 when `encoding` says so. */
    content: string;
    encoding?: "utf8" | "base64";
  };
}

export interface RequestSpec {
  method: Method;
  /** May contain `${captured}` references from an earlier step. */
  path: string;
  body?: Json;
  /** multipart/form-data instead of a JSON body. The boundary is generated;
   *  do not set content-type yourself. */
  form?: FormPart[];
  headers?: Record<string, string>;
  /** Overrides the case's auth for this step alone (an unauthenticated probe
   *  inside an otherwise paired case). */
  auth?: Auth;
}

export interface WsSpec {
  /** Which ticket to present on the upgrade. Anything but "valid" is a
   *  negative case asserting the upgrade is refused. */
  ticket?: "valid" | "none" | "invalid" | "spent";
  /** Expected HTTP status of the upgrade itself. 101 unless stated.
   *  Assertable here because the suite performs the upgrade over a raw TLS
   *  socket; the browser WebSocket API hides this, which is why the headless
   *  E2E lane documents the unauthorized state as uncoverable. */
  status?: number;
  /** Frames to send after a successful upgrade. */
  send?: Json[];
  /** Frames expected back, as an ORDERED SUBSEQUENCE filtered to the kinds
   *  named here. The core interleaves heartbeats and snapshot broadcasts whose
   *  timing is not deterministic, so a full-transcript assertion would be flaky
   *  by construction. */
  expect?: Json[];
}

/** A parity hand-off: stop the core and bring one back on the SAME home, port,
 *  and mock upstream.
 *
 *  - "restart" brings the SAME target back. Proves state survives a reboot.
 *  - "swap" brings the OTHER core up on the same home: the cutover hand-off.
 *    The port is big-bang with no dual-shipping period, so "an install the Deno
 *    core wrote still works when the Rust core boots on it" is the property the
 *    whole migration rests on, and this is the only thing that tests it.
 *
 *  A swap case is not parameterised by `--core`: it exercises both binaries by
 *  construction, so it runs the same way in every mode, always starting on Deno
 *  (the direction the cutover is about, and the only direction that works: a
 *  home seeded for the Rust core lacks three of the four helper binaries the
 *  Deno core refuses to boot without). Both binaries must be present; a missing
 *  one fails the run rather than skipping it. Requires `isolate: true`. */
export type Handoff = "restart" | "swap";

export interface Expect {
  status?: number;
  /** After a hand-off, the SPKI pin must be byte-identical. The TLS keypair is
   *  sealed in the secrets vault and the cert is regenerated every boot, so a
   *  changed pin means the key was not persisted or the derivation drifted.
   *  Either one re-pairs every already-paired client, with no obvious
   *  diagnosis: this is one of the byte-exact parity surfaces. */
  pinUnchanged?: boolean;
  headers?: Record<string, string>;
  /** Exact match, including the key set: an unexpected key is a FAILURE.
   *  Loose-by-default matching is how a port silently drops a field. */
  body?: Json;
  /** Subset match, for collection endpoints whose other contents are not this
   *  case's business. */
  bodyContains?: Json;
  /** JSON paths dropped from the comparison entirely. Every use wants a comment
   *  saying why. */
  ignorePaths?: string[];
  /** { "$.memories": "id" } sorts that array by the key on both sides before
   *  comparing. Only for arrays whose order is genuinely not contractual. */
  unordered?: Record<string, string>;
  /** Ordered SSE payload matchers for a text/event-stream response. */
  sse?: Json[];
  ws?: WsSpec;
  /** Broadcasts expected on the case's watched connection, as a result of this
   *  step. Needs `watch` on the case. Matched as an ordered subsequence of the
   *  kinds named here, waiting up to `frameTimeoutMs`. */
  frames?: Json[];
  /** Assert NO frame of these kinds arrives. Always costs the full bound. */
  noFrames?: string[];
  /** Bound for `frames` / `noFrames`. Defaults to 5s and 1s respectively. */
  frameTimeoutMs?: number;
  /** Byte length of a non-JSON response body (an attachment download). */
  bodyBytes?: number;
  /** sha256 of the raw response body, lowercase hex. The real assertion for an
   *  upload/download round trip: the bytes that came back are the bytes sent. */
  bodySha256?: string;
  timeoutMs?: number;
}

export interface Step {
  request?: RequestSpec;
  /** Stop the core and bring a core back on the same home before this step's
   *  expectation is evaluated. The bearer, the pin, and everything on disk are
   *  expected to carry over untouched. "swap" brings the OTHER core back. */
  handoff?: Handoff;
  expect: Expect;
  /** { sid: "$.id" } pulls a value out of this step's response body and makes
   *  it available as `${sid}` to later steps. This is what lets a case create
   *  the state it reads instead of depending on state it did not create. */
  capture?: Record<string, string>;
}

export interface Case {
  /** Unique, dot-separated, filterable. Convention: `<pkg>.<area>.<what>`. */
  id: string;
  package: PackageId;
  /** The inventory routes this case covers, e.g. "POST /api/v1/sessions".
   *  Required rather than derived from the paths, because the coverage report
   *  is per route and a `${var}` path cannot be mapped back to its template. */
  routes: string[];
  auth: Auth;
  /** Frame kinds to watch for the whole case. The runner opens ONE authenticated
   *  WS connection before the first step and buffers these continuously, so a
   *  step can assert on a broadcast that its own HTTP request triggered.
   *  Requires a paired auth. */
  watch?: string[];
  /** What the mock LLM answers with for this case. Defaults to `echo`, which
   *  streams the last message's content back, so an unscripted chat turn is
   *  deterministic. Reinstalled before every case, so a script cannot leak into
   *  the neighbour sharing the boot. */
  llm?: LlmScript;
  /** Cases sharing a scenario share one core boot, in declaration order. */
  scenario?: string;
  /** Give this case a core of its own. Costs a boot; use for anything that
   *  mutates global state (revoking clients, rotating tokens, admin password).
   *  Required on a cutover case, which replaces the core mid-run. */
  isolate?: boolean;
  /** Boot this case's core on another channel. Defaults to `dev`, which is what
   *  a core run from source is.
   *
   *  Requires `isolate: true`, because the channel is a property of the BOOT and
   *  a scenario shares one. Both cores read `LOCA_CHANNEL` identically, and the
   *  home stays where `LOCA_CORE_HOME` puts it, so the only thing that changes
   *  is what the core believes it is: which is exactly what the
   *  channel-conditional behaviour keys off.
   *
   *  Nothing else in the suite can reach those branches. Until this existed, the
   *  dev-only install gate, the codebase-vs-tarball seeding path and the update
   *  channel were all uncovered on both cores, and each one FAILS OPEN in the
   *  direction of doing more rather than less.
   *
   *  **Off `dev`, a core reaches for the OS keychain**, and that is currently
   *  what stops this being used. `secrets/store.rs`'s `select` keeps the vault
   *  master key in a file on `dev` only; on any other channel the Rust core
   *  talks to the real keychain, and a boot on `stable` was measured HANGING
   *  for the full health budget on a machine that already has a stable install,
   *  because macOS asks a human about an item another binary owns. The Deno
   *  core does not hang, which is worse rather than better: its keychain helper
   *  is a stub in a seeded home, so it silently uses the file store and the two
   *  cores are then not doing the same thing. Setting this needs that answered
   *  first. */
  channel?: "stable" | "dev" | "latest";
  /** Files written into the home before the FIRST core boots.
   *
   *  The escape hatch for a fixture no HTTP step can build, and it exists for
   *  exactly one situation: a cutover case that must assert both cores read the
   *  same on-disk state, where producing that state through the API is either
   *  impossible (a multi-gigabyte model download) or would test the producing
   *  core rather than the agreement.
   *
   *  Requires `isolate: true`, because the home is a property of the BOOT and a
   *  scenario shares one.
   *
   *  **Prefer a step.** A case that plants what it then asserts is a case that
   *  can pass against a core which does nothing, so anything reachable through
   *  the API belongs in a request. See D-168. */
  plant?: PlantedFile[];
  steps: Step[];
}

/** A cutover case: one that hands its home from one core to the other mid-run.
 *
 *  The runner has to recognise these before anything boots, because they are
 *  the one shape that is NOT parameterised by `--core`. They always start on
 *  Deno and finish on the port target, in every mode, and in differential mode
 *  they are run once rather than run twice and compared: a case that swaps,
 *  replayed against both cores, produces two identical transcripts and an
 *  "agree" that means nothing. */
export function isCutoverCase(kase: Case): boolean {
  return kase.steps.some((s) => s.handoff === "swap");
}
