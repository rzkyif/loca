// D15: the speech routes and the providers behind them.
//
// Seven routes, all of them bearer-only. The harness seeds external STT and TTS
// pointed at the loopback mock with no API key (harness/home.ts), which is the
// keyless-loopback path the shared key resolver hands a placeholder to, so the
// relay cases exercise a real request over a real socket without a real
// endpoint or a real key. `/v1/audio/transcriptions` answers
// {"text":"mock transcription"} and `/v1/audio/speech` answers 44 zero bytes
// typed audio/wav (harness/upstream.ts).
//
// Two gaps, both deliberate and neither silenced:
//
//   - CLOSED (D-039). `GET /tts/voices` used to be the suite's one genuine
//     divergence: 53 voices from the oracle, 0 from the port. The cause was not
//     a missing catalog but a misplaced seam, so it did NOT need N3. Voices now
//     come from `TtsVoiceSource`, backed by the model catalog and deliberately
//     kept off the engine trait, so the dropdown fills on a core with no local
//     speech engine at all, which is every mobile build until N3 ships.
//   - Provider-error classification (401 to provider_unauthorized, 429 to
//     provider_rate_limited, and so on) has no case here, because the mock's
//     only scriptable surface is the chat completion path; both audio paths
//     always succeed. It is covered by
//     `speech::tests::a_provider_rejection_keeps_its_code_and_names_the_provider`
//     and `provider_failures_map_onto_stable_codes`.
//
// One tightening is also absent on purpose. `{"speed": "fast"}` is a 400 on the
// Rust core (the generated body type rejects it) and a silently ignored field
// on the Deno core, which reads `speed` straight off an untyped body. That is
// D-026's shape, and a suite that must be green on two implementations can only
// assert their intersection, so it is a Rust route test instead.

import type { Case } from "../runner/types.ts";

const STT = "/api/v1/stt";
const TTS = "/api/v1/tts";

/** A minimal WAV: the mock never decodes it, and both cores relay it whole. */
const CLIP = {
  filename: "clip.wav",
  contentType: "audio/wav",
  content: "RIFF....WAVEfmt ................",
};

/** sha256 of the mock's 44 zero bytes. The real assertion of a synthesis round
 *  trip: the bytes that came back are the bytes the provider sent. */
const SILENT_WAV_SHA256 = "85759b3811ff7dc47b03792ac85317be51431a3f9e01dcafce317ed736a391b0";

export const cases: Case[] = [
  {
    // External transcription has no local engine to poll, so it is running
    // whenever it is the selected provider. The client keys its microphone
    // affordance off this.
    id: "d15.stt.status-reports-the-selected-provider",
    package: "D15",
    routes: ["GET /api/v1/stt/status"],
    auth: "paired",
    scenario: "d15",
    steps: [
      {
        request: { method: "GET", path: `${STT}/status` },
        expect: { status: 200, body: { provider: "external", running: true } },
      },
    ],
  },
  {
    id: "d15.stt.status-needs-a-bearer",
    package: "D15",
    routes: ["GET /api/v1/stt/status"],
    auth: "none",
    scenario: "d15",
    steps: [
      {
        request: { method: "GET", path: `${STT}/status` },
        expect: { status: 401, body: { error: { code: "missing_token", message: "<string>" } } },
      },
    ],
  },
  {
    // The whole external path end to end: multipart in, the provider's
    // transcript out. A core that never made the call cannot pass this.
    id: "d15.stt.transcribe-relays-to-the-external-provider",
    package: "D15",
    routes: ["POST /api/v1/stt/transcribe"],
    auth: "paired",
    scenario: "d15",
    steps: [
      {
        request: {
          method: "POST",
          path: `${STT}/transcribe`,
          form: [
            { name: "audio", file: CLIP },
            { name: "language", value: "es" },
          ],
        },
        expect: { status: 200, body: { text: "mock transcription" } },
      },
    ],
  },
  {
    id: "d15.stt.transcribe-requires-an-audio-file",
    package: "D15",
    routes: ["POST /api/v1/stt/transcribe"],
    auth: "paired",
    scenario: "d15",
    steps: [
      {
        request: {
          method: "POST",
          path: `${STT}/transcribe`,
          form: [{ name: "language", value: "es" }],
        },
        expect: {
          status: 400,
          body: { error: { code: "validation_error", message: "audio file required" } },
        },
      },
    ],
  },
  {
    // A part with no filename is a text field, not a file. Without that rule
    // `audio=hello` reads as a five-byte upload and the provider is paid to
    // transcribe it.
    id: "d15.stt.transcribe-rejects-a-non-file-audio-field",
    package: "D15",
    routes: ["POST /api/v1/stt/transcribe"],
    auth: "paired",
    scenario: "d15",
    steps: [
      {
        request: {
          method: "POST",
          path: `${STT}/transcribe`,
          form: [{ name: "audio", value: "hello" }],
        },
        expect: {
          status: 400,
          body: { error: { code: "validation_error", message: "audio file required" } },
        },
      },
    ],
  },
  {
    id: "d15.stt.transcribe-needs-a-bearer",
    package: "D15",
    routes: ["POST /api/v1/stt/transcribe"],
    auth: "none",
    scenario: "d15",
    steps: [
      {
        request: {
          method: "POST",
          path: `${STT}/transcribe`,
          form: [{ name: "audio", file: CLIP }],
        },
        expect: { status: 401, body: { error: { code: "missing_token", message: "<string>" } } },
      },
    ],
  },
  {
    // Synthesis loads and unloads with the engine, which follows tts.enabled
    // and the model files; these two exist for the calls the client already
    // makes. 204 with no body is the whole contract.
    id: "d15.tts.load-and-unload-are-accepted",
    package: "D15",
    routes: ["POST /api/v1/tts/load", "POST /api/v1/tts/unload"],
    auth: "paired",
    scenario: "d15",
    steps: [
      { request: { method: "POST", path: `${TTS}/load` }, expect: { status: 204, body: null } },
      { request: { method: "POST", path: `${TTS}/unload` }, expect: { status: 204, body: null } },
    ],
  },
  {
    // One engine serves both directions, so it can be up with synthesis
    // switched off. The harness seeds tts.enabled false, and neither core has a
    // speech engine running, so both halves are false for both reasons.
    id: "d15.tts.status-follows-the-enabled-setting",
    package: "D15",
    routes: ["GET /api/v1/tts/status"],
    auth: "paired",
    scenario: "d15",
    steps: [
      {
        request: { method: "GET", path: `${TTS}/status` },
        expect: { status: 200, body: { loaded: false, loading: false } },
      },
    ],
  },
  {
    // A bare array with no envelope, populated from the model catalog.
    // Named for what it now pins. It previously asserted `bodyContains: []`,
    // which any array satisfies including an empty one, so it passed against a
    // core that returned NO voices - which is exactly what the Rust core did
    // until D-039 was closed. A case that cannot fail is worse than no case,
    // because it reads as coverage.
    //
    // `bodyContains` is POSITIONAL on an array, so this pins the first entry.
    // `af_alloy` is the catalog's first Kokoro voice; `af_bella` is the model's
    // DEFAULT voice (`loca-model-catalog/src/tts.ts:151`) and is not first,
    // which is worth writing down because the two are easy to confuse and the
    // first draft of this case asserted the wrong one.
    //
    // It holds on a fresh core with no TTS settings written, and it holds
    // WITHOUT a local speech engine, which is the whole point: voices are a
    // catalog fact, not an engine fact (D-039).
    id: "d15.tts.voices-returns-the-selected-model-voices",
    package: "D15",
    routes: ["GET /api/v1/tts/voices"],
    auth: "paired",
    scenario: "d15",
    steps: [
      {
        request: { method: "GET", path: `${TTS}/voices` },
        expect: {
          status: 200,
          bodyContains: [{ id: "af_alloy" }],
        },
      },
    ],
  },
  {
    // Empty text is a missing field, not an empty job: the declared type says
    // `text: string` and cannot say non-empty, so both cores check it by hand.
    id: "d15.tts.synthesize-rejects-empty-text",
    package: "D15",
    routes: ["POST /api/v1/tts/synthesize"],
    auth: "paired",
    scenario: "d15",
    steps: [
      {
        request: { method: "POST", path: `${TTS}/synthesize`, body: { text: "" } },
        expect: {
          status: 400,
          body: { error: { code: "validation_error", message: "text required" } },
        },
      },
    ],
  },
  {
    // Synthesis time scales with length and the audio is buffered whole, so the
    // bound is enforced before a provider is called. 2001 ASCII characters are
    // over it by every measure; the UTF-16 unit that actually defines it is
    // pinned by `speech::tests::the_text_bound_is_measured_in_utf16_code_units`,
    // which needs an astral string this format cannot carry legibly.
    id: "d15.tts.synthesize-rejects-oversized-text",
    package: "D15",
    routes: ["POST /api/v1/tts/synthesize"],
    auth: "paired",
    scenario: "d15",
    steps: [
      {
        request: {
          method: "POST",
          path: `${TTS}/synthesize`,
          body: { text: "a".repeat(2001) },
        },
        expect: {
          status: 400,
          body: {
            error: { code: "validation_error", message: "tts text exceeds 2000 characters" },
          },
        },
      },
    ],
  },
  {
    // LAST in the scenario: it switches the shared tts.provider to external and
    // leaves it there. The bytes are asserted by digest rather than by length
    // alone, so a core returning 44 bytes of something else fails.
    id: "d15.tts.synthesize-relays-to-the-external-provider",
    package: "D15",
    routes: ["PATCH /api/v1/settings", "POST /api/v1/tts/synthesize"],
    auth: "paired",
    scenario: "d15",
    steps: [
      {
        request: {
          method: "PATCH",
          path: "/api/v1/settings",
          body: { "tts.provider": "external" },
        },
        expect: { status: 200, bodyContains: { "tts.provider": "external" } },
      },
      {
        request: {
          method: "POST",
          path: `${TTS}/synthesize`,
          // A local voice id, which is what the client always sends. The
          // external path must substitute tts.external.voice instead of
          // forwarding it, or the provider 400s on an unknown speaker.
          body: { text: "hello", voice: "af_bella" },
        },
        expect: {
          status: 200,
          headers: { "content-type": "audio/wav" },
          bodyBytes: 44,
          bodySha256: SILENT_WAV_SHA256,
        },
      },
    ],
  },
  {
    // Isolated: it empties a shared settings key and never puts it back.
    id: "d15.stt.external-requires-a-model",
    package: "D15",
    routes: ["PATCH /api/v1/settings", "POST /api/v1/stt/transcribe"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "PATCH",
          path: "/api/v1/settings",
          body: { "stt.external.model": "" },
        },
        expect: { status: 200, bodyContains: { "stt.external.model": "" } },
      },
      {
        request: {
          method: "POST",
          path: `${STT}/transcribe`,
          form: [{ name: "audio", file: CLIP }],
        },
        expect: {
          status: 400,
          body: {
            error: {
              code: "validation_error",
              message: "external STT requires stt.external.baseUrl and stt.external.model",
            },
          },
        },
      },
    ],
  },
  {
    // Isolated, and it never reaches the host it names: the key check happens
    // before any request. The keyless placeholder is scoped to loopback
    // gateways, so a remote endpoint with no key is a configuration error
    // rather than an upstream 401 nobody can read.
    id: "d15.stt.external-requires-a-key-off-loopback",
    package: "D15",
    routes: ["PATCH /api/v1/settings", "POST /api/v1/stt/transcribe"],
    auth: "paired",
    isolate: true,
    steps: [
      {
        request: {
          method: "PATCH",
          path: "/api/v1/settings",
          body: { "stt.external.baseUrl": "https://api.example.com/v1" },
        },
        expect: {
          status: 200,
          bodyContains: { "stt.external.baseUrl": "https://api.example.com/v1" },
        },
      },
      {
        request: {
          method: "POST",
          path: `${STT}/transcribe`,
          form: [{ name: "audio", file: CLIP }],
        },
        expect: {
          status: 400,
          body: {
            error: {
              code: "validation_error",
              message: "external STT requires an API key (stt.external.apiKey)",
            },
          },
        },
      },
    ],
  },
];
