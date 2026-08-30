//! Kokoro voice names to sherpa speaker ids, and the espeak language each one
//! needs.
//!
//! The model catalog carries the same 53 voices and fills the client's
//! dropdown, but this table is the mapping to the integer sherpa addresses a
//! speaker by, and it has to work on an engine that has never seen a catalog.
//! Merging them would make speaker resolution depend on a network-fetched
//! document. `the_catalog_and_the_speaker_table_agree` keeps the two honest: if
//! they diverge the user picks one voice and hears another.

/// Default espeak-ng language for a freshly loaded engine: American English, to
/// match the default `af_bella` voice. A synthesis for a voice in another
/// language reloads the engine in that language.
pub const DEFAULT_LANG: &str = "en-us";

/// Resolve a voice selector to a sherpa speaker id.
///
/// A numeric string wins, because the non-Kokoro families address speakers by
/// index and the catalog gives their voices numeric ids. Then the name table.
/// Then speaker 0, which every family has.
pub fn resolve_sid(voice: &str) -> i32 {
    if let Ok(n) = voice.parse::<i32>() {
        return n;
    }
    VOICES
        .iter()
        .position(|name| *name == voice)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(0)
}

/// The espeak-ng phonemizer language a voice needs, from its leading language
/// letter. Every code returned here must have a dictionary in the bundled
/// espeak-ng-data, because multilingual Kokoro terminates the process on an
/// unknown language, which is why this is a closed match with a fallback.
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

/// The espeak language a synthesis request needs, mirroring the speaker
/// resolution in [`resolve_sid`]: an explicit numeric sid wins over a name.
pub fn request_lang(voice: Option<&str>) -> &'static str {
    match voice {
        Some(selector) => match selector.parse::<i32>().ok().and_then(name_for_sid) {
            Some(name) => voice_lang(name),
            None => voice_lang(selector),
        },
        None => DEFAULT_LANG,
    }
}

/// Speaker names in `voices.bin` order, so the index IS the sherpa speaker id.
/// Reordering this list silently remaps every voice.
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeSet;

    use crate::contract::inbound::{CatalogPayload, TtsFamily};

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
        assert_eq!(voice_lang("unknown"), DEFAULT_LANG);
        assert_eq!(voice_lang(""), DEFAULT_LANG);
    }

    #[test]
    fn every_shipped_voice_resolves_to_a_language_with_bundled_data() {
        // Containment, not tidiness: an espeak code with no dictionary in the
        // bundled data terminates the process at model load.
        const BUNDLED: &[&str] = &[
            "en-us",
            "en-gb-x-rp",
            "es",
            "fr",
            "hi",
            "it",
            "ja",
            "pt-br",
            "cmn",
        ];
        for voice in VOICES {
            let lang = voice_lang(voice);
            assert!(
                BUNDLED.contains(&lang),
                "voice {voice} wants {lang}, which is not in the bundled set",
            );
        }
    }

    #[test]
    fn a_name_resolves_to_its_index_and_an_unknown_name_to_speaker_zero() {
        assert_eq!(resolve_sid("af_alloy"), 0);
        assert_eq!(resolve_sid("af_bella"), 2);
        assert_eq!(resolve_sid("zm_yunyang"), 52);
        assert_eq!(resolve_sid("not_a_voice"), 0);
        // The numeric path, which is how kitten/vits/matcha voices arrive.
        assert_eq!(resolve_sid("7"), 7);
        assert_eq!(resolve_sid("0"), 0);
    }

    #[test]
    fn name_for_sid_resolves_in_range_and_rejects_out_of_range() {
        assert_eq!(name_for_sid(0), Some("af_alloy"));
        assert_eq!(name_for_sid(2), Some("af_bella"));
        assert_eq!(name_for_sid(52), Some("zm_yunyang"));
        assert_eq!(name_for_sid(53), None);
        assert_eq!(name_for_sid(-1), None);
        assert_eq!(name_for_sid(i32::MAX), None);
    }

    #[test]
    fn a_numeric_selector_takes_the_language_of_the_voice_it_names() {
        // "45" is zf_xiaobei, so a numeric request must still reload Mandarin.
        assert_eq!(request_lang(Some("45")), "cmn");
        assert_eq!(request_lang(Some("zf_xiaobei")), "cmn");
        assert_eq!(request_lang(Some("af_bella")), "en-us");
        assert_eq!(request_lang(None), DEFAULT_LANG);
        // Out of range falls back rather than indexing past the table.
        assert_eq!(request_lang(Some("9999")), DEFAULT_LANG);
    }

    /// The SET and the languages must agree; the ORDER deliberately need not.
    /// The catalog groups by language for the dropdown and this table is
    /// `voices.bin` order, and resolution is BY NAME, so nothing may ever
    /// "simplify" `resolve_sid` into a catalog index lookup: the two orders
    /// diverge at position 28. A voice the catalog offers and this table lacks
    /// resolves to speaker 0, with no error anywhere.
    #[test]
    fn the_catalog_and_the_speaker_table_offer_the_same_voices_and_languages() {
        // The file wraps the payload in a provenance envelope, so the payload is
        // reached through `["payload"]` exactly as the models tests do.
        const DEV_CATALOG: &str = include_str!("../../models/catalog.dev.json");
        let file: serde_json::Value =
            serde_json::from_str(DEV_CATALOG).expect("the dev catalog parses");
        let payload: CatalogPayload =
            serde_json::from_value(file["payload"].clone()).expect("the payload deserialises");

        let kokoro = payload
            .tts
            .models
            .iter()
            .find(|model| model.family == TtsFamily::Kokoro)
            .expect("the catalog offers a kokoro model");

        let catalog: BTreeSet<&str> = kokoro.voices.iter().map(|v| v.id.as_str()).collect();
        let table: BTreeSet<&str> = VOICES.iter().copied().collect();
        assert_eq!(
            catalog,
            table,
            "the catalog offers {:?} that the speaker table lacks, and the table has {:?} the catalog does not",
            catalog.difference(&table).collect::<Vec<_>>(),
            table.difference(&catalog).collect::<Vec<_>>(),
        );

        for voice in &kokoro.voices {
            if let Some(lang) = voice.lang.as_deref() {
                assert_eq!(
                    lang,
                    voice_lang(&voice.id),
                    "catalog and table disagree on the language of {}",
                    voice.id,
                );
            }
        }
    }
}
