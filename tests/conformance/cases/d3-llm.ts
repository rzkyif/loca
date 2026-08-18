// D3: the two single-shot LLM utility routes.
//
// `POST /api/v1/llm/autocorrect` and `POST /api/v1/llm/merge` are what the
// Client calls around dictation, and they exist so no LLM traffic bypasses
// Core. Both resolve their endpoint through the same resolver and vault as the
// chat path, so a green case here also proves endpoint resolution, the API-key
// precedence, the request body, the SSE decoding and the trim.
//
// The happy paths are reachable because every core the harness boots is seeded
// with `llm.provider = "external"` pointed at the loopback mock
// (`harness/home.ts`), whose default script echoes the last message's content
// back. So `autocorrect` returns its input, and `merge` returns the exact user
// message the route built: that second one is the only place the merge template
// is observable from the wire, and it is what the merge prompt tells the model
// to expect.
//
// What these deliberately do NOT cover, and why:
//
//   - The LOCAL provider path. Every seeded core is external on purpose, so no
//     sidecar is spawned and no weights are needed, which means the llama.cpp
//     sampler fields, the thinking flag and the scheduler's queue never appear
//     on this wire. All three are covered by Rust tests over the request body
//     and the queue, where a wedged model can be staged directly.
//   - The provider-error mapping. A case cannot script the mock into its error
//     mode from a step, and pointing a core at a closed port would assert a
//     connection failure rather than a provider one. Covered in Rust.
//
// Both cores answer identically on every step below, including the 400s: the
// TypeScript hand-checks the two fields and the Rust deserialises the generated
// `AutocorrectRequest` / `MergeTranscriptionRequest`, and they agree on the
// code as well as the status. Unlike D2 (see D-026) there is no divergence to
// work around here.

import type { Case } from "../runner/types.ts";

export const cases: Case[] = [
  {
    id: "d3.llm.autocorrect.requires-a-paired-client",
    package: "D3",
    routes: ["POST /api/v1/llm/autocorrect"],
    auth: "none",
    scenario: "d3-llm",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/llm/autocorrect",
          body: { text: "helo wrld" },
        },
        expect: {
          status: 401,
          body: { error: { code: "missing_token", message: "<string>" } },
        },
      },
    ],
  },
  {
    id: "d3.llm.autocorrect.rejects-a-missing-text",
    package: "D3",
    routes: ["POST /api/v1/llm/autocorrect"],
    auth: "paired",
    scenario: "d3-llm",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/llm/autocorrect", body: {} },
        expect: {
          status: 400,
          body: { error: { code: "validation_error", message: "<string>" } },
        },
      },
    ],
  },
  {
    id: "d3.llm.autocorrect.rejects-an-empty-text",
    package: "D3",
    routes: ["POST /api/v1/llm/autocorrect"],
    auth: "paired",
    scenario: "d3-llm",
    steps: [
      {
        // The one rule the contract's plain `string` cannot express, so both
        // cores enforce it by hand. An empty transcription is a round trip to
        // the model with nothing to correct.
        request: { method: "POST", path: "/api/v1/llm/autocorrect", body: { text: "" } },
        expect: {
          status: 400,
          body: { error: { code: "validation_error", message: "<string>" } },
        },
      },
    ],
  },
  {
    id: "d3.llm.autocorrect.rejects-a-wrong-typed-text",
    package: "D3",
    routes: ["POST /api/v1/llm/autocorrect"],
    auth: "paired",
    scenario: "d3-llm",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/llm/autocorrect", body: { text: 42 } },
        expect: {
          status: 400,
          body: { error: { code: "validation_error", message: "<string>" } },
        },
      },
    ],
  },
  {
    id: "d3.llm.autocorrect.returns-the-completion",
    package: "D3",
    routes: ["POST /api/v1/llm/autocorrect"],
    auth: "paired",
    scenario: "d3-llm",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/llm/autocorrect",
          body: { text: "helo wrld" },
        },
        // The mock echoes the last message, which is the user turn, so the
        // input coming back proves the route sent it as the user message rather
        // than folding it into the system prompt.
        expect: { status: 200, body: { text: "helo wrld" } },
      },
    ],
  },
  {
    id: "d3.llm.merge.requires-a-paired-client",
    package: "D3",
    routes: ["POST /api/v1/llm/merge"],
    auth: "none",
    scenario: "d3-llm",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/llm/merge",
          body: { existing: "one", next: "two" },
        },
        expect: {
          status: 401,
          body: { error: { code: "missing_token", message: "<string>" } },
        },
      },
    ],
  },
  {
    id: "d3.llm.merge.rejects-a-missing-next",
    package: "D3",
    routes: ["POST /api/v1/llm/merge"],
    auth: "paired",
    scenario: "d3-llm",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/llm/merge", body: { existing: "one" } },
        expect: {
          status: 400,
          body: { error: { code: "validation_error", message: "<string>" } },
        },
      },
    ],
  },
  {
    id: "d3.llm.merge.rejects-an-empty-next",
    package: "D3",
    routes: ["POST /api/v1/llm/merge"],
    auth: "paired",
    scenario: "d3-llm",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/llm/merge",
          body: { existing: "one", next: "" },
        },
        expect: {
          status: 400,
          body: { error: { code: "validation_error", message: "<string>" } },
        },
      },
    ],
  },
  {
    id: "d3.llm.merge.wraps-both-texts-in-the-template",
    package: "D3",
    routes: ["POST /api/v1/llm/merge"],
    auth: "paired",
    scenario: "d3-llm",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/llm/merge",
          body: { existing: "one", next: "two" },
        },
        // The echo makes the user message visible, and the tags are the whole
        // contract between this route and its prompt: the default merge prompt
        // tells the model it will receive EXISTING and NEW, so a port that
        // changed the wrapper would leave the prompt describing something the
        // model never sees.
        expect: {
          status: 200,
          body: { text: "<existing>\none\n</existing>\n<new>\ntwo\n</new>" },
        },
      },
    ],
  },
  {
    id: "d3.llm.merge.accepts-an-empty-existing",
    package: "D3",
    routes: ["POST /api/v1/llm/merge"],
    auth: "paired",
    scenario: "d3-llm",
    steps: [
      {
        // A blank input box is the ordinary first dictation, not an error. Only
        // `next` is required.
        request: {
          method: "POST",
          path: "/api/v1/llm/merge",
          body: { existing: "", next: "two" },
        },
        expect: {
          status: 200,
          body: { text: "<existing>\n\n</existing>\n<new>\ntwo\n</new>" },
        },
      },
    ],
  },
];
