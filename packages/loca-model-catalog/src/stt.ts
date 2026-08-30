// Speech-to-Text catalog: the sherpa-onnx offline-recognizer bundles Loca
// offers, served by the core's in-process speech engine.
//
// Sources (verified against the HuggingFace API tree on 2026-06-15): each
// model's files + exact byte sizes come from its csukuangfj mirror. A model is a
// bundle of role-tagged files whose roles depend on the family (whisper:
// encoder/decoder/tokens; CTC families: model/tokens; transducer:
// encoder/decoder/joiner/tokens; moonshine: four onnx + tokens). The catalog
// lists every file explicitly, so the download flow and the sidecar never guess
// filenames; selection bakes the chosen bundle into stt.modelFiles.
//
// Curation: int8 is the shipped quant everywhere (near-lossless at a fraction of
// the fp32 size). A handful of families are curated cards (see presets); the
// rest are reachable through the Custom model browser.

import type { SpeechQuant, SttCatalog, SttCatalogModel, SttFamily } from "@loca/shared";

type RoleFile = [role: string, modelSpec: string, fileSizeBytes: number];

/** One quant from a list of [role, spec, size]; the badge size is their sum. */
function quant(quantName: string, files: RoleFile[]): SpeechQuant {
  return {
    quant: quantName,
    files: files.map(([role, modelSpec, fileSizeBytes]) => ({
      role,
      modelSpec,
      fileSizeBytes,
    })),
    fileSizeBytes: files.reduce((sum, [, , bytes]) => sum + bytes, 0),
  };
}

function stt(
  id: string,
  name: string,
  family: SttFamily,
  english: boolean,
  quants: SpeechQuant[],
): SttCatalogModel {
  return { id, name, family, english, quants };
}

const WHISPER = "csukuangfj/sherpa-onnx-whisper";

/** A whisper int8 bundle. `m` names both the repo suffix and the file prefix. */
function whisper(
  id: string,
  name: string,
  english: boolean,
  m: string,
  enc: number,
  dec: number,
  tok: number,
): SttCatalogModel {
  return stt(id, name, "whisper", english, [
    quant("int8", [
      ["encoder", `@${WHISPER}-${m}/main/${m}-encoder.int8.onnx`, enc],
      ["decoder", `@${WHISPER}-${m}/main/${m}-decoder.int8.onnx`, dec],
      ["tokens", `@${WHISPER}-${m}/main/${m}-tokens.txt`, tok],
    ]),
  ]);
}

/** A single-file CTC/AED family (model.int8.onnx + tokens.txt in one repo). */
function singleFile(
  id: string,
  name: string,
  family: SttFamily,
  english: boolean,
  repo: string,
  modelBytes: number,
  tokensBytes: number,
): SttCatalogModel {
  return stt(id, name, family, english, [
    quant("int8", [
      ["model", `@${repo}/main/model.int8.onnx`, modelBytes],
      ["tokens", `@${repo}/main/tokens.txt`, tokensBytes],
    ]),
  ]);
}

export const STT_CATALOG: SttCatalog = {
  models: [
    // Whisper: encoder + decoder + tokens. `.en` builds transcribe English
    // slightly better than their multilingual siblings of the same size.
    whisper(
      "whisper-tiny.en",
      "Whisper Tiny (English)",
      true,
      "tiny.en",
      12_937_772,
      89_853_865,
      835_554,
    ),
    whisper("whisper-tiny", "Whisper Tiny", false, "tiny", 12_937_772, 89_855_401, 816_730),
    whisper(
      "whisper-base.en",
      "Whisper Base (English)",
      true,
      "base.en",
      29_120_534,
      130_669_978,
      835_554,
    ),
    whisper("whisper-base", "Whisper Base", false, "base", 29_120_534, 130_672_026, 816_730),
    whisper(
      "whisper-small.en",
      "Whisper Small (English)",
      true,
      "small.en",
      112_442_483,
      262_223_042,
      835_554,
    ),
    whisper("whisper-small", "Whisper Small", false, "small", 112_442_483, 262_226_114, 816_730),
    whisper(
      "whisper-medium.en",
      "Whisper Medium (English)",
      true,
      "medium.en",
      374_196_283,
      571_055_161,
      835_554,
    ),
    whisper(
      "whisper-turbo",
      "Whisper Large v3 Turbo",
      false,
      "turbo",
      674_716_297,
      361_080_764,
      816_730,
    ),

    // SenseVoice: non-autoregressive, very fast on CPU; zh/yue/en/ja/ko.
    singleFile(
      "sense-voice",
      "SenseVoice",
      "sense-voice",
      false,
      "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17",
      239_233_841,
      315_894,
    ),

    // Moonshine: four onnx files + tokens; English, strong on short clips.
    stt("moonshine-tiny.en", "Moonshine Tiny (English)", "moonshine", true, [
      quant("int8", [
        [
          "preprocessor",
          "@csukuangfj/sherpa-onnx-moonshine-tiny-en-int8/main/preprocess.onnx",
          6_800_738,
        ],
        [
          "encoder",
          "@csukuangfj/sherpa-onnx-moonshine-tiny-en-int8/main/encode.int8.onnx",
          18_249_187,
        ],
        [
          "uncached_decoder",
          "@csukuangfj/sherpa-onnx-moonshine-tiny-en-int8/main/uncached_decode.int8.onnx",
          53_216_096,
        ],
        [
          "cached_decoder",
          "@csukuangfj/sherpa-onnx-moonshine-tiny-en-int8/main/cached_decode.int8.onnx",
          45_264_830,
        ],
        ["tokens", "@csukuangfj/sherpa-onnx-moonshine-tiny-en-int8/main/tokens.txt", 436_688],
      ]),
    ]),
    stt("moonshine-base.en", "Moonshine Base (English)", "moonshine", true, [
      quant("int8", [
        [
          "preprocessor",
          "@csukuangfj/sherpa-onnx-moonshine-base-en-int8/main/preprocess.onnx",
          14_077_290,
        ],
        [
          "encoder",
          "@csukuangfj/sherpa-onnx-moonshine-base-en-int8/main/encode.int8.onnx",
          50_311_494,
        ],
        [
          "uncached_decoder",
          "@csukuangfj/sherpa-onnx-moonshine-base-en-int8/main/uncached_decode.int8.onnx",
          122_120_451,
        ],
        [
          "cached_decoder",
          "@csukuangfj/sherpa-onnx-moonshine-base-en-int8/main/cached_decode.int8.onnx",
          99_983_837,
        ],
        ["tokens", "@csukuangfj/sherpa-onnx-moonshine-base-en-int8/main/tokens.txt", 436_688],
      ]),
    ]),

    // Paraformer: non-autoregressive English offline model.
    singleFile(
      "paraformer-en",
      "Paraformer (English)",
      "paraformer",
      true,
      "csukuangfj/sherpa-onnx-paraformer-en-2024-03-09",
      229_772_488,
      46_872,
    ),

    // NeMo CTC: small English conformer, very light.
    singleFile(
      "nemo-ctc-en-small",
      "NeMo Conformer CTC Small (English)",
      "nemo-ctc",
      true,
      "csukuangfj/sherpa-onnx-nemo-ctc-en-conformer-small",
      46_419_854,
      11_611,
    ),

    // Zipformer transducer: encoder + decoder + joiner + tokens (English).
    stt("zipformer-en", "Zipformer Transducer (English)", "transducer", true, [
      quant("int8", [
        [
          "encoder",
          "@csukuangfj/sherpa-onnx-zipformer-en-2023-06-26/main/encoder-epoch-99-avg-1.int8.onnx",
          68_778_564,
        ],
        [
          "decoder",
          "@csukuangfj/sherpa-onnx-zipformer-en-2023-06-26/main/decoder-epoch-99-avg-1.int8.onnx",
          1_307_236,
        ],
        [
          "joiner",
          "@csukuangfj/sherpa-onnx-zipformer-en-2023-06-26/main/joiner-epoch-99-avg-1.int8.onnx",
          259_335,
        ],
        ["tokens", "@csukuangfj/sherpa-onnx-zipformer-en-2023-06-26/main/tokens.txt", 5_048],
      ]),
    ]),
    stt("zipformer-small-en", "Zipformer Transducer Small (English)", "transducer", true, [
      quant("int8", [
        [
          "encoder",
          "@csukuangfj/sherpa-onnx-zipformer-small-en-2023-06-26/main/encoder-epoch-99-avg-1.int8.onnx",
          26_015_366,
        ],
        [
          "decoder",
          "@csukuangfj/sherpa-onnx-zipformer-small-en-2023-06-26/main/decoder-epoch-99-avg-1.int8.onnx",
          1_307_236,
        ],
        [
          "joiner",
          "@csukuangfj/sherpa-onnx-zipformer-small-en-2023-06-26/main/joiner-epoch-99-avg-1.int8.onnx",
          259_335,
        ],
        ["tokens", "@csukuangfj/sherpa-onnx-zipformer-small-en-2023-06-26/main/tokens.txt", 5_048],
      ]),
    ]),

    // Dolphin: 40 Eastern/Asian languages + Chinese dialects, light footprint.
    singleFile(
      "dolphin-base",
      "Dolphin Base (Multilingual)",
      "dolphin",
      false,
      "csukuangfj/sherpa-onnx-dolphin-base-ctc-multi-lang-int8-2025-04-02",
      103_729_802,
      504_662,
    ),

    // TeleSpeech: Chinese + dialects.
    singleFile(
      "telespeech-zh",
      "TeleSpeech CTC (Chinese)",
      "telespeech-ctc",
      false,
      "csukuangfj/sherpa-onnx-telespeech-ctc-int8-zh-2024-06-04",
      340_990_949,
      66_642,
    ),
  ],
  // Curated cards: light English-only, light multilingual, the accurate turbo
  // model, and SenseVoice as a fast multilingual option. Everything else is
  // reachable through the Custom model browser.
  presets: [
    { id: "light-english", modelId: "whisper-small.en", quant: "int8" },
    { id: "light-multilingual", modelId: "whisper-small", quant: "int8" },
    { id: "accurate", modelId: "whisper-turbo", quant: "int8" },
    { id: "fast-multilingual", modelId: "sense-voice", quant: "int8" },
  ],
};
