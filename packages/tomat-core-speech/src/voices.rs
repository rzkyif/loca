//! Maps Tomat's named Kokoro voices (e.g. "af_bella") to sherpa speaker ids for
//! the bundled `kokoro-int8-multi-lang-v1_0` model.
//!
//! The order below is authoritative: it mirrors sherpa-onnx's
//! `scripts/kokoro/v1.0/generate_voices_bin.py`, which writes the speaker style
//! vectors to voices.bin in exactly this order, so index == sherpa sid. It also
//! matches the `tts.voice` enum in the shared settings schema one-for-one.

/// Resolve a voice selector to a sherpa speaker id. Accepts a numeric id passed
/// as a string, or a known voice name; falls back to speaker 0.
pub fn resolve_sid(voice: &str) -> i32 {
    if let Ok(n) = voice.parse::<i32>() {
        return n;
    }
    VOICES
        .iter()
        .position(|name| *name == voice)
        .map(|i| i as i32)
        .unwrap_or(0)
}

/// Default espeak-ng language for a freshly-loaded engine: American English, to
/// match the default `af_bella` voice. A /speak for a voice in another language
/// reloads the engine in that language (see `voice_lang`).
pub const DEFAULT_LANG: &str = "en-us";

/// The espeak-ng phonemizer language a voice needs, from its leading language
/// letter (every Kokoro id starts with one: a/b English, e Spanish, f French,
/// h Hindi, i Italian, j Japanese, p Brazilian Portuguese, z Mandarin). The
/// multilingual model bakes this language in at load time, so the server reloads
/// the engine when a requested voice changes language. Every code returned here
/// has a dictionary in the bundled espeak-ng-data, so none can fail the model's
/// frontend init (an unknown code would).
pub fn voice_lang(voice: &str) -> &'static str {
    match voice.as_bytes().first() {
        Some(b'a') => "en-us",      // American English
        Some(b'b') => "en-gb-x-rp", // British English (the bare "en-gb" alias aborts espeak)
        Some(b'e') => "es",         // Spanish
        Some(b'f') => "fr",         // French
        Some(b'h') => "hi",         // Hindi
        Some(b'i') => "it",         // Italian
        Some(b'j') => "ja",         // Japanese
        Some(b'p') => "pt-br",      // Brazilian Portuguese
        Some(b'z') => "cmn",        // Mandarin Chinese
        _ => DEFAULT_LANG,
    }
}

/// Voice name for a sherpa speaker id, when it is in range.
pub fn name_for_sid(sid: i32) -> Option<&'static str> {
    usize::try_from(sid)
        .ok()
        .and_then(|i| VOICES.get(i))
        .copied()
}

/// Speaker names in voices.bin order (index == sherpa sid).
static VOICES: &[&str] = &[
    "af_alloy",
    "af_aoede",
    "af_bella",
    "af_heart",
    "af_jessica",
    "af_kore",
    "af_nicole",
    "af_nova",
    "af_river",
    "af_sarah",
    "af_sky",
    "am_adam",
    "am_echo",
    "am_eric",
    "am_fenrir",
    "am_liam",
    "am_michael",
    "am_onyx",
    "am_puck",
    "am_santa",
    "bf_alice",
    "bf_emma",
    "bf_isabella",
    "bf_lily",
    "bm_daniel",
    "bm_fable",
    "bm_george",
    "bm_lewis",
    "ef_dora",
    "em_alex",
    "ff_siwis",
    "hf_alpha",
    "hf_beta",
    "hm_omega",
    "hm_psi",
    "if_sara",
    "im_nicola",
    "jf_alpha",
    "jf_gongitsune",
    "jf_nezumi",
    "jf_tebukuro",
    "jm_kumo",
    "pf_dora",
    "pm_alex",
    "pm_santa",
    "zf_xiaobei",
    "zf_xiaoni",
    "zf_xiaoxiao",
    "zf_xiaoyi",
    "zm_yunjian",
    "zm_yunxi",
    "zm_yunxia",
    "zm_yunyang",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_lang_maps_each_language_prefix() {
        assert_eq!(voice_lang("af_bella"), "en-us");
        assert_eq!(voice_lang("bm_george"), "en-gb-x-rp");
        assert_eq!(voice_lang("ef_dora"), "es");
        assert_eq!(voice_lang("ff_siwis"), "fr");
        assert_eq!(voice_lang("hf_alpha"), "hi");
        assert_eq!(voice_lang("if_sara"), "it");
        assert_eq!(voice_lang("jf_alpha"), "ja");
        assert_eq!(voice_lang("pm_alex"), "pt-br");
        assert_eq!(voice_lang("zf_xiaobei"), "cmn");
        // Unknown / empty selectors fall back rather than yielding a bad code.
        assert_eq!(voice_lang("unknown"), DEFAULT_LANG);
        assert_eq!(voice_lang(""), DEFAULT_LANG);
    }

    #[test]
    fn every_shipped_voice_resolves_to_a_language() {
        // The multilingual model exits the process on an unknown espeak language,
        // so every voice we expose must map to a code with a bundled dictionary.
        for v in VOICES {
            assert!(!voice_lang(v).is_empty(), "voice {v} has no language");
        }
    }

    #[test]
    fn name_for_sid_resolves_in_range_and_rejects_out_of_range() {
        assert_eq!(name_for_sid(0), Some("af_alloy"));
        assert_eq!(name_for_sid(2), Some("af_bella"));
        assert_eq!(name_for_sid(-1), None);
        assert_eq!(name_for_sid(i32::MAX), None);
    }
}
