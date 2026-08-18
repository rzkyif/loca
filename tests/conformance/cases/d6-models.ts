// D6: models. The catalog, the on-device fit engine, and the model manager.
//
// Seventeen routes under /api/v1/models. Thirteen are covered here; what the
// other four wait on, and why several assertions are placeholders rather than
// values, is below.
//
// WHAT THESE DELIBERATELY DO NOT ASSERT, AND WHY:
//
//   - POST /probe, POST /download and POST /ensure need the download manager,
//     which is D7's. The seam refuses rather than answering empty, because an
//     empty probe reads as "already downloaded" and an empty enqueue as
//     "nothing to do", and both are silent. Until D7 lands they are covered by
//     Rust route tests against a fake queue, which also cover the batch bounds
//     and the group defaulting.
//
//   - WHICH model the fit engine picks. That is a function of the host's RAM,
//     so a case asserting "the Full bucket is Qwen3.5-9B" would pass on the
//     machine it was written on and nowhere else. The arithmetic is pinned by
//     30 Rust tests against fixed synthetic hardware, six of them sitting
//     exactly on a threshold with the comparison operator mutated to watch them
//     go red. What belongs here is the ENVELOPE: that a bucket is present as
//     null rather than absent, that Full is the only bucket with idle-unload,
//     that flashAttn is on, that quality is a number.
//
//   - `footprintBytes`, `fits`, `quant` and `recommended` on a browser row and
//     on each of its quants. Every one of them is a function of THIS host's
//     memory budget, so no concrete value can be right on two machines, and a
//     placeholder is the strongest assertion available. That is a real blind
//     spot and it is worth naming rather than leaving implicit: a placeholder
//     is MASKED in diff mode (D-029), so the cross-core comparison does not see
//     these fields at all. The two constants they hang on (`budgetFraction.full`
//     and the Full context ceiling, both read by `config_for_quant`) are pinned
//     instead by `models::fit::tests::a_manual_quant_pick_is_budgeted_at_the_full_fraction_and_the_full_ceiling`,
//     against fixed synthetic hardware where an exact figure IS meaningful.
//     Everything on a row that is signed catalog data rather than a per-device
//     answer is asserted exactly here: the id, family, name, score, parameter
//     count, tags, variant label, projector path, and every quant's `modelSpec`
//     in catalog order. The speech cards are exact end to end, since no fit
//     engine runs on them.
//
//   - HARDWARE VALUES, which are asserted with placeholders on purpose (D-029).
//     There is one known difference between the two cores here and it is a
//     harness artefact, not a port divergence: the Deno core's boot check is
//     satisfied with an EMPTY stub `loca-core-hwinfo` (harness/home.ts), so its
//     helper spawn fails and it falls back to guessing the GPU from the
//     platform, reporting `gpu.name: "Apple GPU"`. The Rust core absorbed that
//     helper (D-025), so it reports the real CPU brand. Every other field
//     agrees. Asserting `<string>` keeps diff mode quiet about the harness
//     while still requiring the field to be present and typed.
//
// TWO ASSUMPTIONS ABOUT THE HOST, stated because a failure here would look like
// a port bug and would not be one:
//
//   - It has enough RAM that all three buckets resolve. A device under about
//     6 GB legitimately returns nulls, and no single expectation can be right
//     for both.
//   - It has at least 8 cores, so `threads` saturates at the cap on both cores
//     despite the Deno oracle counting logical cores through its fallback and
//     the Rust core counting physical ones.

import type { Case } from "../runner/types.ts";

/** The `llm.*` shape every selection applies. Machine-dependent fields carry a
 *  placeholder; the rest are contractual and asserted exactly. */
const APPLIED_MODEL = {
  modelPath: "<string>",
  mmprojPath: "<string>",
  contextSize: "<number>",
  threads: "<number>",
  gpuLayers: "<number>",
  flashAttn: true,
  supportImages: "<bool>",
  idleUnloadSeconds: 0,
  reasoningBudget: "<number>",
  temperature: "<number>",
  topP: "<number>",
  topK: "<number>",
  minP: "<number>",
  repeatPenalty: "<number>",
  dryMultiplier: "<number>",
  presencePenalty: "<number>",
};

/** One bucket of a recommendation. */
const BUCKET = {
  bucket: "<string>",
  modelId: "<string>",
  name: "<string>",
  quality: "<number>",
  quant: "<string>",
  footprintBytes: "<number>",
  vision: "<bool>",
  variantLabel: "<string>",
  apply: { ...APPLIED_MODEL, samplers: "<any>", mmprojPath: "<any>" },
};

const HARDWARE = {
  totalRamBytes: "<number>",
  availableRamBytes: "<number>",
  cpuCoresPhysical: "<number>",
  // See the header: the oracle is running without its hwinfo helper.
  gpu: { backend: "<string>", name: "<string>", vramBytes: "<number>" },
  unifiedMemory: "<bool>",
};

/** The whisper bundle behind the `light-multilingual` card, exactly as the
 *  setting stores it: COMPACT, and in the catalog's file order rather than
 *  sorted. Sorting would put `decoder` first, which is what serialising a map
 *  would do on the Rust side, so this string is the assertion that the two
 *  cores write the same bytes into `stt.modelFiles`. */
const STT_BUNDLE =
  '{"encoder":"@csukuangfj/sherpa-onnx-whisper-small/main/small-encoder.int8.onnx",' +
  '"decoder":"@csukuangfj/sherpa-onnx-whisper-small/main/small-decoder.int8.onnx",' +
  '"tokens":"@csukuangfj/sherpa-onnx-whisper-small/main/small-tokens.txt"}';

/** The same shape for TTS, which is PRETTY-PRINTED with two spaces where STT is
 *  compact. The asymmetry reads as an accident in the TypeScript
 *  (`stt.ts:64` against `tts.ts:48`) and it is the specification: both values
 *  are persisted settings that real installs already carry. */
const TTS_BUNDLE = [
  "{",
  '  "model": "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/model.int8.onnx",',
  '  "voices": "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/voices.bin",',
  '  "tokens": "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/tokens.txt"',
  "}",
].join("\n");

export const cases: Case[] = [
  // --- the manager ---------------------------------------------------------

  {
    id: "d6.models.list.empty",
    package: "D6",
    routes: ["GET /api/v1/models"],
    auth: "paired",
    scenario: "d6-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/models" },
        expect: { status: 200, body: [] },
      },
    ],
  },
  {
    id: "d6.models.list.requires-a-paired-client",
    package: "D6",
    routes: ["GET /api/v1/models"],
    auth: "none",
    scenario: "d6-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/models" },
        expect: { status: 401, bodyContains: { error: { code: "missing_token" } } },
      },
    ],
  },
  {
    id: "d6.models.delete.not-on-disk",
    package: "D6",
    routes: ["DELETE /api/v1/models/:relPath"],
    auth: "paired",
    scenario: "d6-read",
    steps: [
      {
        request: { method: "DELETE", path: "/api/v1/models/nobody/nothing/model.gguf" },
        expect: { status: 404, bodyContains: { error: { code: "model_not_found" } } },
      },
    ],
  },

  // --- the download queue, which is D7-backed but mounted here -------------

  {
    id: "d6.models.downloads.empty",
    package: "D6",
    routes: ["GET /api/v1/models/downloads"],
    auth: "paired",
    scenario: "d6-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/models/downloads" },
        expect: { status: 200, body: [] },
      },
    ],
  },
  {
    // All three are no-ops for an id the queue does not know, on both cores.
    // That is worth pinning: a port that 404s here would break the client's
    // optimistic cancel.
    id: "d6.models.downloads.actions-on-an-unknown-id",
    package: "D6",
    routes: [
      "POST /api/v1/models/downloads/:id/cancel",
      "POST /api/v1/models/downloads/:id/retry",
      "DELETE /api/v1/models/downloads/:id",
    ],
    auth: "paired",
    scenario: "d6-read",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/models/downloads/no-such-job/cancel" },
        expect: { status: 204 },
      },
      {
        request: { method: "POST", path: "/api/v1/models/downloads/no-such-job/retry" },
        expect: { status: 204 },
      },
      {
        request: { method: "DELETE", path: "/api/v1/models/downloads/no-such-job" },
        expect: { status: 204 },
      },
      // And the catch-all model delete did not swallow any of them: it is still
      // there for a real path.
      {
        request: { method: "DELETE", path: "/api/v1/models/u/r/f.gguf" },
        expect: { status: 404, bodyContains: { error: { code: "model_not_found" } } },
      },
    ],
  },
  {
    // `modelDownloadBodySchema` and `modelProbeBodySchema` are `.strict()` with
    // a `.min(1)` on every source. Both constraints reach Rust only because the
    // schemas live in `@loca/shared/validation`: while they were route-local
    // the bridge could not see them, the port fell back to the constraint-free
    // TypeScript shape, and `additionalProperties` was stripped from that half
    // on purpose (`relaxRequestBodies`), so an unknown key was accepted here
    // and refused by the oracle. See D-093.
    //
    // Every body below is refused before anything is enqueued, so no case here
    // starts a download.
    id: "d6.models.batch-bodies-are-strict",
    package: "D6",
    routes: ["POST /api/v1/models/download", "POST /api/v1/models/probe"],
    auth: "paired",
    scenario: "d6-read",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/models/download",
          body: { items: [], notAField: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/probe",
          body: { sources: [], notAField: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // Strict reaches the array's items too.
        request: {
          method: "POST",
          path: "/api/v1/models/download",
          body: { items: [{ source: "u/r/f.gguf", notAField: true }] },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/download",
          body: { items: [{ source: "" }] },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/models/probe", body: { sources: [""] } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/download",
          body: { items: [{ source: "u/r/f.gguf", group: "video" }] },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        // The queue is still empty: nothing above got as far as enqueuing.
        request: { method: "GET", path: "/api/v1/models/downloads" },
        expect: { status: 200, body: [] },
      },
    ],
  },

  // --- the adaptive presets ------------------------------------------------

  {
    id: "d6.models.recommend",
    package: "D6",
    routes: ["GET /api/v1/models/recommend"],
    auth: "paired",
    scenario: "d6-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/models/recommend" },
        expect: {
          status: 200,
          bodyContains: {
            hardware: HARDWARE,
            catalogGeneratedAt: "<string>",
            // Nothing has been selected in this scenario, so the client has
            // nothing to diff against.
            applied: {},
            buckets: {
              smallest: {
                ...BUCKET,
                bucket: "smallest",
                apply: { ...BUCKET.apply, idleUnloadSeconds: 0 },
              },
              half: { ...BUCKET, bucket: "half", apply: { ...BUCKET.apply, idleUnloadSeconds: 0 } },
              // Full is the only bucket that unloads when idle, because it is
              // the one sized to hurt if it stays resident.
              full: {
                ...BUCKET,
                bucket: "full",
                apply: { ...BUCKET.apply, idleUnloadSeconds: 300 },
              },
            },
          },
        },
      },
    ],
  },
  {
    // Non-destructive: it re-fetches the catalog and re-probes hardware, and on
    // the dev channel the catalog is compiled from source, so the only
    // observable change is a fresh timestamp.
    id: "d6.models.recommend.recheck",
    package: "D6",
    routes: ["POST /api/v1/models/recommend/recheck"],
    auth: "paired",
    scenario: "d6-read",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/models/recommend/recheck" },
        expect: {
          status: 200,
          bodyContains: {
            hardware: HARDWARE,
            catalogGeneratedAt: "<string>",
            applied: {},
            buckets: {
              smallest: { ...BUCKET, bucket: "smallest" },
              half: { ...BUCKET, bucket: "half" },
              full: {
                ...BUCKET,
                bucket: "full",
                apply: { ...BUCKET.apply, idleUnloadSeconds: 300 },
              },
            },
          },
        },
      },
    ],
  },

  // --- the browser and the speech pickers ----------------------------------

  {
    id: "d6.models.catalog",
    package: "D6",
    routes: ["GET /api/v1/models/catalog"],
    auth: "paired",
    scenario: "d6-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/models/catalog" },
        expect: {
          status: 200,
          bodyContains: {
            generatedAt: "<string>",
            models: [
              {
                // The first family's first model. The catalog is signed data
                // rather than a per-host answer, so the identity, the score and
                // the parameter count are exact; only how it LANDS on this
                // device is not.
                id: "Qwen/Qwen3.6-27B",
                family: "Qwen 3.6",
                name: "Qwen3.6 27B",
                quality: 45.8,
                paramsB: 27,
                vision: true,
                tags: [],
                quant: "<string>",
                variantLabel: "standard",
                footprintBytes: "<number>",
                fits: "<bool>",
                apply: {
                  ...APPLIED_MODEL,
                  // Set even though `supportImages` is off: the path is ready
                  // for the moment the user opts in, and setting it is not what
                  // turns vision on.
                  mmprojPath: "@unsloth/Qwen3.6-27B-GGUF/main/mmproj-F16.gguf",
                  samplers: "<any>",
                },
                quants: [
                  {
                    modelSpec: "@unsloth/Qwen3.6-27B-GGUF/main/Qwen3.6-27B-Q8_0.gguf",
                    quant: "Q8_0",
                    variantLabel: "standard",
                    footprintBytes: "<number>",
                    fits: "<bool>",
                    recommended: "<bool>",
                  },
                  {
                    modelSpec: "@unsloth/Qwen3.6-27B-GGUF/main/Qwen3.6-27B-Q4_K_M.gguf",
                    quant: "Q4_K_M",
                    variantLabel: "standard",
                    footprintBytes: "<number>",
                    fits: "<bool>",
                    recommended: "<bool>",
                  },
                  {
                    modelSpec: "@unsloth/Qwen3.6-27B-GGUF/main/Qwen3.6-27B-Q3_K_M.gguf",
                    quant: "Q3_K_M",
                    variantLabel: "standard",
                    footprintBytes: "<number>",
                    fits: "<bool>",
                    recommended: "<bool>",
                  },
                  {
                    modelSpec: "@unsloth/Qwen3.6-27B-GGUF/main/Qwen3.6-27B-UD-Q2_K_XL.gguf",
                    quant: "UD-Q2_K_XL",
                    variantLabel: "standard",
                    footprintBytes: "<number>",
                    fits: "<bool>",
                    recommended: "<bool>",
                  },
                ],
              },
            ],
          },
        },
      },
    ],
  },
  {
    id: "d6.models.stt.catalog",
    package: "D6",
    routes: ["GET /api/v1/models/stt/catalog"],
    auth: "paired",
    scenario: "d6-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/models/stt/catalog" },
        expect: {
          status: 200,
          bodyContains: {
            generatedAt: "<string>",
            // The curated cards, resolved against the catalog. There is no fit
            // engine here: every bundle fits every device, so these are exact.
            presets: [
              {
                id: "light-english",
                modelId: "whisper-small.en",
                name: "Whisper Small (English)",
                family: "whisper",
                english: true,
                quant: "int8",
                modelSpec:
                  "@csukuangfj/sherpa-onnx-whisper-small.en/main/small.en-encoder.int8.onnx",
                fileSizeBytes: 375501079,
              },
              {
                id: "light-multilingual",
                modelId: "whisper-small",
                name: "Whisper Small",
                family: "whisper",
                english: false,
                quant: "int8",
                modelSpec: "@csukuangfj/sherpa-onnx-whisper-small/main/small-encoder.int8.onnx",
                fileSizeBytes: 375485327,
              },
              {
                id: "accurate",
                modelId: "whisper-turbo",
                name: "Whisper Large v3 Turbo",
                family: "whisper",
                english: false,
                quant: "int8",
                modelSpec: "@csukuangfj/sherpa-onnx-whisper-turbo/main/turbo-encoder.int8.onnx",
                fileSizeBytes: 1036613791,
              },
              {
                // The one card whose family is not whisper, so its primary file
                // is `model` rather than `encoder`. That mapping is the whole
                // reason `SttFamily::primary_role` exists.
                id: "fast-multilingual",
                modelId: "sense-voice",
                name: "SenseVoice",
                family: "sense-voice",
                english: false,
                quant: "int8",
                modelSpec:
                  "@csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/main/model.int8.onnx",
                fileSizeBytes: 239549735,
              },
            ],
          },
        },
      },
    ],
  },
  {
    id: "d6.models.tts.catalog",
    package: "D6",
    routes: ["GET /api/v1/models/tts/catalog"],
    auth: "paired",
    scenario: "d6-read",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/models/tts/catalog" },
        expect: {
          status: 200,
          bodyContains: {
            generatedAt: "<string>",
            presets: [
              {
                id: "kokoro",
                modelId: "kokoro",
                name: "Kokoro Multilingual",
                family: "kokoro",
                quant: "int8",
                modelSpec: "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/model.int8.onnx",
                fileSizeBytes: 141977461,
                // The card carries its model's voices, so the voice dropdown
                // populates from the selection with no second round trip.
                voices: "<any>",
                defaultVoice: "af_bella",
              },
              {
                id: "kitten",
                modelId: "kitten",
                name: "Kitten Nano (English)",
                family: "kitten",
                quant: "fp16",
                modelSpec: "@csukuangfj/kitten-nano-en-v0_1-fp16/main/model.fp16.onnx",
                fileSizeBytes: 23857842,
                voices: "<any>",
                defaultVoice: "1",
              },
              {
                id: "piper",
                modelId: "piper-amy",
                name: "Piper Amy (American English)",
                family: "vits",
                quant: "fp32",
                modelSpec: "@csukuangfj/vits-piper-en_US-amy-low/main/en_US-amy-low.onnx",
                fileSizeBytes: 63105420,
                voices: "<any>",
                defaultVoice: "0",
              },
              {
                // Matcha splits its acoustic model from its vocoder, so its
                // primary file is `acoustic_model` where every other family
                // here uses `model`. The spec below is the acoustic one.
                id: "matcha",
                modelId: "matcha-ljspeech",
                name: "Matcha LJSpeech (English)",
                family: "matcha",
                quant: "fp32",
                modelSpec: "@csukuangfj/matcha-icefall-en_US-ljspeech/main/model-steps-3.onnx",
                fileSizeBytes: 77908348,
                voices: "<any>",
                defaultVoice: "0",
              },
            ],
          },
        },
      },
    ],
  },

  // --- selection, which writes settings ------------------------------------
  //
  // Their own scenario, after the read-only ones: every case here mutates the
  // settings the recommend cases assert are untouched.

  {
    // A specific model by id, which is deterministic on any host: the browser
    // always has a row for every catalog model, fitting or not. Bucket
    // selection takes the same path one function earlier and is covered by the
    // Rust route tests, where the hardware is fixed.
    id: "d6.models.select.by-model-id",
    package: "D6",
    routes: ["POST /api/v1/models/select"],
    auth: "paired",
    scenario: "d6-select",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/models/select",
          body: { modelId: "Qwen/Qwen3.5-4B" },
        },
        expect: {
          status: 200,
          bodyContains: {
            applied: {
              preset: "custom",
              settings: {
                ...APPLIED_MODEL,
                // Qwen ships its own tuning, and selecting a model applies it
                // rather than leaving the previous model's values in place.
                temperature: 0.6,
                topK: 20,
                repeatPenalty: 1,
                mmprojPath: "@unsloth/Qwen3.5-4B-GGUF/main/mmproj-F16.gguf",
                samplers: "<any>",
              },
            },
          },
        },
      },
      {
        // The write landed, and it landed in the right store. The sampling keys
        // are `client-on-core`, so they come back through this client's own
        // overlay rather than being written into every other client's baseline.
        request: { method: "GET", path: "/api/v1/settings" },
        expect: {
          status: 200,
          bodyContains: {
            "llm.provider": "local",
            "llm.preset": "custom",
            "llm.modelPath": "<string>",
            "llm.mmprojPath": "@unsloth/Qwen3.5-4B-GGUF/main/mmproj-F16.gguf",
            "llm.contextSize": "<number>",
            "llm.temperature": 0.6,
            "llm.topK": 20,
            // ABSENT, not false. `GET /settings` returns the sparse store, so
            // a key nobody wrote is missing entirely, and that is the stronger
            // assertion: selecting a vision-capable model must not turn vision
            // on, because doing so would add a projector download as a side
            // effect of picking a model.
            "llm.supportImages": "<absent>",
          },
        },
      },
      {
        // And the recommendation now reports what the settings point at, which
        // is what lets the client flag a better option.
        request: { method: "GET", path: "/api/v1/models/recommend" },
        expect: {
          status: 200,
          bodyContains: {
            applied: { preset: "custom", modelPath: "<string>" },
            // Named rather than omitted: an unasserted field is compared raw
            // between the two cores in diff mode, and all three of these differ
            // per run or per host. `gpu.name` is the harness artefact the header
            // describes (the oracle runs without its hwinfo helper and reports
            // "Apple GPU"; the Rust core detects the real chip), and it would
            // otherwise read as a port divergence. See D-029.
            hardware: HARDWARE,
            catalogGeneratedAt: "<string>",
          },
        },
      },
    ],
  },
  {
    id: "d6.models.select.rejects-an-empty-or-unknown-choice",
    package: "D6",
    routes: ["POST /api/v1/models/select"],
    auth: "paired",
    scenario: "d6-select",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/models/select", body: {} },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/select",
          body: { bucket: "enormous" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/select",
          body: { modelSpec: "@nobody/nothing/main/x.gguf" },
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/select",
          body: { modelId: "nobody/nothing" },
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        // D-098, and the exact body the `--core=diff` step that found it used:
        // the oracle answered 400 and the port 200. Zod's `.optional()` is
        // `T | undefined` and REFUSES `null`; serde's `Option<T>` accepted one
        // and mapped it to `None`, so `bucket: null` fell through the handler's
        // three truthiness checks and selected nothing while reporting success.
        //
        // It sits here rather than in a case of its own because this route is
        // where it was measured, and it belongs beside the `{}` step above: the
        // two bodies are the same request as far as the handler is concerned,
        // so they must get the same answer.
        request: {
          method: "POST",
          path: "/api/v1/models/select",
          body: { bucket: null },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    // The bundle string is the assertion. It is persisted verbatim, read back
    // by the speech engine and by the download flow, and both its key ORDER
    // (the catalog's, not sorted) and its FORM (compact) are load-bearing.
    id: "d6.models.stt.select",
    package: "D6",
    routes: ["POST /api/v1/models/stt/select"],
    auth: "paired",
    scenario: "d6-select",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/models/stt/select",
          body: { presetId: "light-multilingual" },
        },
        expect: {
          status: 200,
          body: {
            applied: {
              preset: "light-multilingual",
              settings: {
                modelType: "whisper",
                modelPath: "@csukuangfj/sherpa-onnx-whisper-small/main/small-encoder.int8.onnx",
                modelFiles: STT_BUNDLE,
                threads: "<number>",
              },
            },
          },
        },
      },
      {
        request: { method: "GET", path: "/api/v1/settings" },
        expect: {
          status: 200,
          bodyContains: {
            "stt.provider": "local",
            "stt.preset": "light-multilingual",
            "stt.modelType": "whisper",
            "stt.modelFiles": STT_BUNDLE,
          },
        },
      },
    ],
  },
  {
    id: "d6.models.tts.select",
    package: "D6",
    routes: ["POST /api/v1/models/tts/select"],
    auth: "paired",
    scenario: "d6-select",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/models/tts/select",
          body: { presetId: "kokoro" },
        },
        expect: {
          status: 200,
          body: {
            applied: {
              preset: "kokoro",
              settings: {
                modelType: "kokoro",
                modelPath: "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/model.int8.onnx",
                // Two-space indented, where the STT bundle above is compact.
                modelFiles: TTS_BUNDLE,
                voice: "af_bella",
              },
            },
          },
        },
      },
      {
        request: { method: "GET", path: "/api/v1/settings" },
        expect: {
          status: 200,
          bodyContains: {
            "tts.preset": "kokoro",
            "tts.modelType": "kokoro",
            "tts.modelFiles": TTS_BUNDLE,
            // ABSENT from the read, and present on disk. `tts.voice` is a
            // `client-on-client` key, so this route writes it into the SHARED
            // store (the settings writer persists whatever it is handed, with
            // no destination check) and `GET /settings` then filters it back
            // out, because that read returns only the two core-stored
            // destinations. Both cores do exactly this. The write itself is
            // asserted by a Rust route test reading the store directly, which
            // is the only place it is visible.
            "tts.voice": "<absent>",
          },
        },
      },
    ],
  },
  {
    // An empty string is not a choice on any of the three select routes. The
    // route schemas are `z.string().optional()` with no `.min(1)`, so `""`
    // passes validation and reaches the handler on both cores, and the handler
    // then SKIPS it and lets the next discriminator decide. A client that
    // clears a field rather than deleting the key sends exactly this, and the
    // Rust core answered 400 or 404 for it until D-043.
    //
    // The pairs below each carry an empty higher-precedence field and a real
    // lower-precedence one, so a core that treats `""` as present cannot reach
    // the 200: it looks up the empty value and misses.
    id: "d6.models.select.an-empty-field-falls-through",
    package: "D6",
    routes: [
      "POST /api/v1/models/select",
      "POST /api/v1/models/stt/select",
      "POST /api/v1/models/tts/select",
    ],
    auth: "paired",
    scenario: "d6-select",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/models/select",
          body: { bucket: "", modelId: "Qwen/Qwen3.5-4B" },
        },
        expect: { status: 200, bodyContains: { applied: { preset: "custom" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/select",
          body: { modelSpec: "", modelId: "Qwen/Qwen3.5-4B" },
        },
        expect: { status: 200, bodyContains: { applied: { preset: "custom" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/stt/select",
          body: { presetId: "", modelId: "whisper-small" },
        },
        expect: {
          status: 200,
          bodyContains: {
            applied: { preset: "custom", settings: { modelFiles: STT_BUNDLE } },
          },
        },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/tts/select",
          body: { presetId: "", modelSpec: "", modelId: "kokoro" },
        },
        expect: {
          status: 200,
          bodyContains: {
            applied: { preset: "custom", settings: { modelFiles: TTS_BUNDLE } },
          },
        },
      },
      {
        // All of them empty is the same body as all of them absent: nothing was
        // chosen, which is a validation error rather than a lookup miss.
        request: {
          method: "POST",
          path: "/api/v1/models/select",
          body: { bucket: "", modelId: "", modelSpec: "" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/stt/select",
          body: { presetId: "", modelId: "", modelSpec: "" },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    // The three select bodies are `.strict()` too. No divergence to fix here,
    // but the constraint now arrives from the generated type rather than from a
    // hand-written stand-in, and this is what would notice it going missing.
    id: "d6.models.select-bodies-are-strict",
    package: "D6",
    routes: [
      "POST /api/v1/models/select",
      "POST /api/v1/models/stt/select",
      "POST /api/v1/models/tts/select",
    ],
    auth: "paired",
    scenario: "d6-select",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/models/select",
          body: { modelId: "Qwen/Qwen3.5-4B", notAField: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/stt/select",
          body: { modelId: "whisper-small", notAField: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
      {
        request: {
          method: "POST",
          path: "/api/v1/models/tts/select",
          body: { modelId: "kokoro", notAField: true },
        },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "d6.models.speech-select.rejects-an-unknown-preset",
    package: "D6",
    routes: ["POST /api/v1/models/stt/select", "POST /api/v1/models/tts/select"],
    auth: "paired",
    scenario: "d6-select",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/models/stt/select",
          body: { presetId: "no-such-card" },
        },
        expect: { status: 404, bodyContains: { error: { code: "not_found" } } },
      },
      {
        request: { method: "POST", path: "/api/v1/models/tts/select", body: {} },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
];
