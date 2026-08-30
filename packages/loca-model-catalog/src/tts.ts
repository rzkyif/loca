// Text-to-Speech catalog: the sherpa-onnx offline-TTS bundles Loca offers,
// served by the core's in-process speech engine. Mirrors the STT catalog shape.
//
// Sources (verified against the HuggingFace API tree on 2026-06-15). A model is
// a bundle of role-tagged files whose roles depend on the family (kokoro/kitten:
// model/voices/tokens; vits: model/tokens; matcha: acoustic_model/vocoder/
// tokens). espeak-ng phonemizer data is NOT a download: it ships inside the
// speech binary and the sidecar passes it as the data_dir for every family.
// selection bakes the chosen bundle into tts.modelFiles.

import type { SpeechQuant, TtsCatalog, TtsCatalogModel, TtsFamily, TtsVoice } from "@loca/shared";

type RoleFile = [role: string, modelSpec: string, fileSizeBytes: number];

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

function tts(
  id: string,
  name: string,
  family: TtsFamily,
  voices: TtsVoice[],
  defaultVoice: string,
  quants: SpeechQuant[],
): TtsCatalogModel {
  return { id, name, family, voices, defaultVoice, quants };
}

// --- Kokoro voices (53). Ids line up with voices.bin speaker order; the espeak
// language per voice (the prefix's locale) lets the sidecar reload the engine
// when a voice crosses languages. ---
const KOKORO_LANG: Record<string, string> = {
  a: "en-us",
  b: "en-gb-x-rp",
  e: "es",
  f: "fr",
  h: "hi",
  i: "it",
  j: "ja",
  p: "pt-br",
  z: "cmn",
};
function kokoro(id: string, label: string): TtsVoice {
  return { id, label, lang: KOKORO_LANG[id[0]] };
}
const KOKORO_VOICES: TtsVoice[] = [
  kokoro("af_alloy", "American • Female • Alloy"),
  kokoro("af_aoede", "American • Female • Aoede"),
  kokoro("af_bella", "American • Female • Bella"),
  kokoro("af_heart", "American • Female • Heart"),
  kokoro("af_jessica", "American • Female • Jessica"),
  kokoro("af_kore", "American • Female • Kore"),
  kokoro("af_nicole", "American • Female • Nicole"),
  kokoro("af_nova", "American • Female • Nova"),
  kokoro("af_river", "American • Female • River"),
  kokoro("af_sarah", "American • Female • Sarah"),
  kokoro("af_sky", "American • Female • Sky"),
  kokoro("am_adam", "American • Male • Adam"),
  kokoro("am_echo", "American • Male • Echo"),
  kokoro("am_eric", "American • Male • Eric"),
  kokoro("am_fenrir", "American • Male • Fenrir"),
  kokoro("am_liam", "American • Male • Liam"),
  kokoro("am_michael", "American • Male • Michael"),
  kokoro("am_onyx", "American • Male • Onyx"),
  kokoro("am_puck", "American • Male • Puck"),
  kokoro("am_santa", "American • Male • Santa"),
  kokoro("bf_alice", "British • Female • Alice"),
  kokoro("bf_emma", "British • Female • Emma"),
  kokoro("bf_isabella", "British • Female • Isabella"),
  kokoro("bf_lily", "British • Female • Lily"),
  kokoro("bm_daniel", "British • Male • Daniel"),
  kokoro("bm_fable", "British • Male • Fable"),
  kokoro("bm_george", "British • Male • George"),
  kokoro("bm_lewis", "British • Male • Lewis"),
  kokoro("jf_alpha", "Japanese • Female • Alpha"),
  kokoro("jf_gongitsune", "Japanese • Female • Gongitsune"),
  kokoro("jf_nezumi", "Japanese • Female • Nezumi"),
  kokoro("jf_tebukuro", "Japanese • Female • Tebukuro"),
  kokoro("jm_kumo", "Japanese • Male • Kumo"),
  kokoro("zf_xiaobei", "Mandarin • Female • Xiaobei"),
  kokoro("zf_xiaoni", "Mandarin • Female • Xiaoni"),
  kokoro("zf_xiaoxiao", "Mandarin • Female • Xiaoxiao"),
  kokoro("zf_xiaoyi", "Mandarin • Female • Xiaoyi"),
  kokoro("zm_yunjian", "Mandarin • Male • Yunjian"),
  kokoro("zm_yunxi", "Mandarin • Male • Yunxi"),
  kokoro("zm_yunxia", "Mandarin • Male • Yunxia"),
  kokoro("zm_yunyang", "Mandarin • Male • Yunyang"),
  kokoro("ef_dora", "Spanish • Female • Dora"),
  kokoro("em_alex", "Spanish • Male • Alex"),
  kokoro("ff_siwis", "French • Female • Siwis"),
  kokoro("hf_alpha", "Hindi • Female • Alpha"),
  kokoro("hf_beta", "Hindi • Female • Beta"),
  kokoro("hm_omega", "Hindi • Male • Omega"),
  kokoro("hm_psi", "Hindi • Male • Psi"),
  kokoro("if_sara", "Italian • Female • Sara"),
  kokoro("im_nicola", "Italian • Male • Nicola"),
  kokoro("pf_dora", "Brazilian Portuguese • Female • Dora"),
  kokoro("pm_alex", "Brazilian Portuguese • Male • Alex"),
  kokoro("pm_santa", "Brazilian Portuguese • Male • Santa"),
];

// --- Kitten voices (8). Ids are the numeric speaker indices in voices.bin order
// (expr-voice-2-m, -2-f, -3-m, ... -5-f), so the sidecar's numeric sid path maps
// them directly; only Kokoro uses named voices. All English (espeak en-us). ---
function kitten(id: string, label: string): TtsVoice {
  return { id, label, lang: "en-us" };
}
const KITTEN_VOICES: TtsVoice[] = [
  kitten("0", "English • Male • Jasper"),
  kitten("1", "English • Female • Bella"),
  kitten("2", "English • Male • Bruno"),
  kitten("3", "English • Female • Luna"),
  kitten("4", "English • Male • Hugo"),
  kitten("5", "English • Female • Rosie"),
  kitten("6", "English • Male • Leo"),
  kitten("7", "English • Female • Kiki"),
];

// Single-speaker families (vits/matcha) expose one voice; sherpa addresses it by
// speaker id 0.
const SINGLE_VOICE: TtsVoice[] = [{ id: "0", label: "Default" }];

/** A single-speaker Piper (vits) voice: model.onnx + tokens.txt. */
function piper(
  id: string,
  name: string,
  repo: string,
  model: string,
  modelBytes: number,
  tokensBytes: number,
): TtsCatalogModel {
  return tts(id, name, "vits", SINGLE_VOICE, "0", [
    quant("fp32", [
      ["model", `@${repo}/main/${model}`, modelBytes],
      ["tokens", `@${repo}/main/tokens.txt`, tokensBytes],
    ]),
  ]);
}

export const TTS_CATALOG: TtsCatalog = {
  models: [
    tts("kokoro", "Kokoro Multilingual", "kokoro", KOKORO_VOICES, "af_bella", [
      quant("int8", [
        ["model", "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/model.int8.onnx", 114_298_054],
        ["voices", "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/voices.bin", 27_678_720],
        ["tokens", "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/tokens.txt", 687],
      ]),
    ]),
    tts("kitten", "Kitten Nano (English)", "kitten", KITTEN_VOICES, "1", [
      quant("fp16", [
        ["model", "@csukuangfj/kitten-nano-en-v0_1-fp16/main/model.fp16.onnx", 23_848_586],
        ["voices", "@csukuangfj/kitten-nano-en-v0_1-fp16/main/voices.bin", 8_192],
        ["tokens", "@csukuangfj/kitten-nano-en-v0_1-fp16/main/tokens.txt", 1_064],
      ]),
    ]),
    piper(
      "piper-amy",
      "Piper Amy (American English)",
      "csukuangfj/vits-piper-en_US-amy-low",
      "en_US-amy-low.onnx",
      63_104_657,
      763,
    ),
    piper(
      "piper-alan",
      "Piper Alan (British English)",
      "csukuangfj/vits-piper-en_GB-alan-medium",
      "en_GB-alan-medium.onnx",
      63_201_430,
      921,
    ),
    piper(
      "piper-ryan",
      "Piper Ryan (American English)",
      "csukuangfj/vits-piper-en_US-ryan-medium",
      "en_US-ryan-medium.onnx",
      63_201_425,
      921,
    ),
    tts("matcha-ljspeech", "Matcha LJSpeech (English)", "matcha", SINGLE_VOICE, "0", [
      quant("fp32", [
        [
          "acoustic_model",
          "@csukuangfj/matcha-icefall-en_US-ljspeech/main/model-steps-3.onnx",
          74_157_680,
        ],
        ["vocoder", "@csukuangfj/sherpa-onnx-hifigan/main/hifigan_v2.onnx", 3_749_714],
        ["tokens", "@csukuangfj/matcha-icefall-en_US-ljspeech/main/tokens.txt", 954],
      ]),
    ]),
  ],
  // All four families are curated cards; the extra Piper voices live in Custom.
  presets: [
    { id: "kokoro", modelId: "kokoro", quant: "int8" },
    { id: "kitten", modelId: "kitten", quant: "fp16" },
    { id: "piper", modelId: "piper-amy", quant: "fp32" },
    { id: "matcha", modelId: "matcha-ljspeech", quant: "fp32" },
  ],
};
