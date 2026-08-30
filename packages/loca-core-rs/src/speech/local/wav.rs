//! WAV in and WAV out, both in Rust.
//!
//! Decoding here rather than through sherpa's `Wave::read` is containment: its
//! Rust wrapper does `CString::new(filename).unwrap()`, its C++ side can die by
//! an unhandled exception crossing the FFI boundary, and going through it would
//! cost a temp-file round trip of up to 25 MB per call. Decoding here keeps
//! attacker-controlled bytes out of the C++ entirely.
//!
//! BOTH values that cross are validated, and the second is easy to forget: the
//! samples, and the SAMPLE RATE, which is client-controlled on the same call
//! and is cast to `i32` on the way in, so an unbounded one wraps negative. See
//! [`MIN_SAMPLE_RATE`].
//!
//! `hound` does the container parsing and returns `Result` for every malformed
//! input; what is written here is what this product accepts and the downmix to
//! the mono the models want.

use std::io::Cursor;

use crate::error::{validation, Result};

/// The sample rates a decoded clip may declare. Containment, and the upper half
/// is the point: the rate crosses into sherpa as an `i32`, so a 55-byte WAV
/// declaring 4,000,000,000 Hz decodes cleanly and arrives as -294967296, into a
/// feature extractor that can terminate the process. The range covers
/// everything real, from 8 kHz telephony to 384 kHz hi-res, with room either
/// side.
const MIN_SAMPLE_RATE: u32 = 1_000;
const MAX_SAMPLE_RATE: u32 = 384_000;

/// The sample rate a synthesis answer claims when the engine reports a
/// nonsensical one. Kokoro is 24 kHz and every other bundled family reports its
/// own rate, so this is a floor for an engine that answered zero rather than a
/// default anybody should hit.
const FALLBACK_SAMPLE_RATE: u32 = 24_000;

/// A decoded clip: mono `f32` samples in [-1, 1] and the rate they were sampled
/// at.
pub struct Clip {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Decode an uploaded clip to mono `f32`. Accepts PCM at 8, 16, 24 or 32 bits
/// and IEEE float at 32 bits, mono or multi channel; anything else is a
/// `validation_error` naming what was wrong. Multi-channel input is AVERAGED
/// rather than left-channel-picked: a clip recorded with the microphone on the
/// right channel would otherwise transcribe as silence.
pub fn decode(bytes: &[u8]) -> Result<Clip> {
    let reader = hound::WavReader::new(Cursor::new(bytes))
        .map_err(|err| validation(format!("could not read the audio: {err}")))?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err(validation("the audio declares no channels"));
    }
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&spec.sample_rate) {
        return Err(validation(format!(
            "the audio declares a sample rate of {} Hz, outside the supported {MIN_SAMPLE_RATE} to {MAX_SAMPLE_RATE} Hz",
            spec.sample_rate,
        )));
    }

    let interleaved = read_samples(reader, spec)?;
    let channels = usize::from(spec.channels);
    if interleaved.is_empty() {
        return Err(validation("the audio contains no samples"));
    }

    let samples = if channels == 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect()
    };

    Ok(Clip {
        samples,
        sample_rate: spec.sample_rate,
    })
}

/// Read every sample as `f32` in [-1, 1]. hound yields the stored
/// representation, so the normalisation is ours: the divisor is the full-scale
/// magnitude for the stored bit depth, which is what makes a 24-bit and a
/// 16-bit clip of the same sound produce the same numbers.
fn read_samples<R: std::io::Read>(
    mut reader: hound::WavReader<R>,
    spec: hound::WavSpec,
) -> Result<Vec<f32>> {
    let bad = |err: hound::Error| validation(format!("could not decode the audio: {err}"));
    match spec.sample_format {
        hound::SampleFormat::Float => {
            if spec.bits_per_sample != 32 {
                return Err(validation(format!(
                    "{}-bit float audio is not supported; use 32-bit float or PCM",
                    spec.bits_per_sample,
                )));
            }
            reader
                .samples::<f32>()
                .map(|s| s.map_err(bad))
                .collect::<Result<Vec<f32>>>()
        }
        hound::SampleFormat::Int => {
            // 8-bit WAV is stored UNSIGNED with a 128 offset that hound has
            // already applied, so one scale per depth is all that is left.
            let scale = match spec.bits_per_sample {
                8 => 128.0,
                16 => 32_768.0,
                24 => 8_388_608.0,
                32 => 2_147_483_648.0,
                other => {
                    return Err(validation(format!(
                        "{other}-bit audio is not supported; use 8, 16, 24 or 32-bit PCM",
                    )))
                }
            };
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale).map_err(bad))
                .collect::<Result<Vec<f32>>>()
        }
    }
}

/// Encode mono `f32` samples as a 16-bit PCM WAV, byte-for-byte the canonical
/// 44-byte RIFF layout: the extension broker reads the sample rate straight out
/// of byte offset 24 and the client's playback queue consumes whole WAV bytes,
/// so a different chunk layout would still be valid and would break both.
pub fn encode(samples: &[f32], sample_rate: i32) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let rate = u32::try_from(sample_rate).unwrap_or(FALLBACK_SAMPLE_RATE);
    let rate = if rate == 0 {
        FALLBACK_SAMPLE_RATE
    } else {
        rate
    };
    let mut buf = Vec::with_capacity(44 + data_len);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // format = PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // channels = mono
    buf.extend_from_slice(&rate.to_le_bytes());
    // Saturating: a nonsense rate above `u32::MAX / 2` would otherwise panic
    // here in a debug build.
    buf.extend_from_slice(&rate.saturating_mul(2).to_le_bytes()); // byte rate (mono)
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_len as u32).to_le_bytes());
    for &sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        buf.extend_from_slice(&((clamped * 32767.0) as i16).to_le_bytes());
    }
    buf
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A WAV built by hound, so the fixtures are not built by the code under
    /// test: one written with our own `encode` would let two bugs cancel.
    fn wav(
        spec: hound::WavSpec,
        write: impl FnOnce(&mut hound::WavWriter<Cursor<&mut Vec<u8>>>),
    ) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut writer = hound::WavWriter::new(Cursor::new(&mut out), spec).unwrap();
            write(&mut writer);
            writer.finalize().unwrap();
        }
        out
    }

    fn pcm16(rate: u32, channels: u16) -> hound::WavSpec {
        hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        }
    }

    #[test]
    fn a_16k_mono_clip_decodes_to_normalized_samples() {
        // What the client actually sends: float32ToWav(samples, 16000).
        let bytes = wav(pcm16(16_000, 1), |w| {
            for value in [0i16, 16_384, -16_384, 32_767] {
                w.write_sample(value).unwrap();
            }
        });
        let clip = decode(&bytes).unwrap();
        assert_eq!(clip.sample_rate, 16_000);
        assert_eq!(clip.samples.len(), 4);
        assert!((clip.samples[0] - 0.0).abs() < 1e-6);
        assert!((clip.samples[1] - 0.5).abs() < 1e-3);
        assert!((clip.samples[2] + 0.5).abs() < 1e-3);
        assert!((clip.samples[3] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn a_stereo_clip_is_averaged_rather_than_left_channel_picked() {
        // Silence on the left, signal on the right. Picking channel 0 would
        // transcribe this as silence, which is a plausible microphone setup.
        let bytes = wav(pcm16(16_000, 2), |w| {
            for _ in 0..4 {
                w.write_sample(0i16).unwrap();
                w.write_sample(32_767i16).unwrap();
            }
        });
        let clip = decode(&bytes).unwrap();
        assert_eq!(
            clip.samples.len(),
            4,
            "one sample per frame, not per channel"
        );
        for sample in clip.samples {
            assert!(
                (sample - 0.5).abs() < 1e-3,
                "expected the average, got {sample}"
            );
        }
    }

    #[test]
    fn every_supported_bit_depth_normalizes_to_the_same_scale() {
        // Half of full scale at each depth must decode to about 0.5, which is
        // what makes the per-depth divisor load-bearing rather than decorative.
        let cases: Vec<(u32, i32)> =
            vec![(8, 64), (16, 16_384), (24, 4_194_304), (32, 1_073_741_824)];
        for (bits, value) in cases {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: bits as u16,
                sample_format: hound::SampleFormat::Int,
            };
            let bytes = wav(spec, |w| {
                w.write_sample(value).unwrap();
            });
            let clip = decode(&bytes).unwrap();
            assert!(
                (clip.samples[0] - 0.5).abs() < 1e-2,
                "{bits}-bit decoded to {}",
                clip.samples[0],
            );
        }
    }

    #[test]
    fn a_32_bit_float_clip_decodes() {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 22_050,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let bytes = wav(spec, |w| {
            w.write_sample(0.25f32).unwrap();
        });
        let clip = decode(&bytes).unwrap();
        assert_eq!(clip.sample_rate, 22_050);
        assert!((clip.samples[0] - 0.25).abs() < 1e-6);
    }

    /// The containment property, stated as a test: every one of these is a byte
    /// sequence a client can upload, and each must come back as a validation
    /// error rather than reaching sherpa.
    #[test]
    fn malformed_input_is_refused_rather_than_forwarded() {
        for (name, bytes) in [
            ("empty", Vec::new()),
            ("not riff", b"this is not a wav file at all".to_vec()),
            ("riff header only", b"RIFF\x24\x00\x00\x00WAVE".to_vec()),
            (
                "truncated mid-header",
                b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec(),
            ),
            ("mp3 magic", vec![0xff, 0xfb, 0x90, 0x00, 0x00, 0x00]),
        ] {
            let err = match decode(&bytes) {
                Ok(_) => panic!("{name} decoded instead of being refused"),
                Err(err) => err,
            };
            assert_eq!(
                err.code,
                crate::error::ErrorCode::ValidationError,
                "{name} produced the wrong code",
            );
        }
    }

    /// The rate is client-controlled and crosses into sherpa as an `i32`, so an
    /// unbounded one wraps NEGATIVE: 4,000,000,000 arrives at `accept_waveform`
    /// as -294967296 and `u32::MAX` as -1, from a 55-byte file.
    #[test]
    fn an_out_of_range_sample_rate_is_refused_before_it_can_wrap_negative() {
        for rate in [4_000_000_000u32, u32::MAX, MAX_SAMPLE_RATE + 1, 999, 1] {
            // Built by hand: hound refuses to WRITE some of these, which is
            // exactly why the reader has to defend itself.
            let bytes = hand_built_wav(rate);
            let err = match decode(&bytes) {
                Ok(clip) => panic!(
                    "rate {rate} was accepted and reaches sherpa as {}",
                    clip.sample_rate as i32,
                ),
                Err(err) => err,
            };
            assert_eq!(
                err.code,
                crate::error::ErrorCode::ValidationError,
                "rate {rate}"
            );
            assert!(
                err.message.contains("sample rate"),
                "rate {rate} gave {}",
                err.message,
            );
        }
    }

    #[test]
    fn the_rates_real_audio_uses_are_accepted() {
        // The bound must not reject anything anybody actually records.
        for rate in [
            MIN_SAMPLE_RATE,
            8_000,
            16_000,
            44_100,
            48_000,
            192_000,
            MAX_SAMPLE_RATE,
        ] {
            let clip = decode(&hand_built_wav(rate))
                .unwrap_or_else(|err| panic!("rate {rate} was refused: {}", err.message));
            assert_eq!(clip.sample_rate, rate);
            // And it survives the cast the engine performs.
            assert!(clip.sample_rate as i32 > 0, "rate {rate} wrapped negative");
        }
    }

    /// A minimal 8-bit mono PCM WAV with one sample, declaring `rate`.
    /// Hand-built because `hound::WavWriter` validates the spec it is given, and
    /// that refusal is a property of the writer rather than the reader.
    fn hand_built_wav(rate: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(45);
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&37u32.to_le_bytes());
        buf.extend_from_slice(b"WAVEfmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
        buf.extend_from_slice(&1u16.to_le_bytes()); // mono
        buf.extend_from_slice(&rate.to_le_bytes());
        buf.extend_from_slice(&rate.to_le_bytes()); // byte rate (8-bit mono)
        buf.extend_from_slice(&1u16.to_le_bytes()); // block align
        buf.extend_from_slice(&8u16.to_le_bytes()); // bits per sample
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.push(128);
        buf
    }

    #[test]
    fn a_clip_with_no_samples_is_refused() {
        let bytes = wav(pcm16(16_000, 1), |_| {});
        let err = match decode(&bytes) {
            Ok(_) => panic!("an empty clip decoded"),
            Err(err) => err,
        };
        assert_eq!(err.message, "the audio contains no samples");
    }

    #[test]
    fn the_encoded_header_is_the_layout_the_broker_and_the_client_read() {
        let bytes = encode(&[0.0, 1.0, -1.0], 24_000);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(&bytes[36..40], b"data");
        // The broker reads the rate from offset 24 and nowhere else.
        assert_eq!(
            u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            24_000
        );
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 1, "mono");
        assert_eq!(u16::from_le_bytes([bytes[34], bytes[35]]), 16, "16-bit");
        assert_eq!(
            u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]),
            48_000,
            "byte rate"
        );
        assert_eq!(bytes.len(), 44 + 3 * 2);
        // RIFF size counts everything after the first eight bytes.
        assert_eq!(
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize,
            bytes.len() - 8,
        );
        assert_eq!(
            u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize,
            3 * 2,
        );
    }

    #[test]
    fn encoding_clamps_rather_than_wrapping() {
        // An engine that overshoots produces samples outside [-1, 1], and
        // without the clamp the cast wraps loud audio into inverted noise.
        let bytes = encode(&[2.0, -2.0], 24_000);
        assert_eq!(i16::from_le_bytes([bytes[44], bytes[45]]), 32_767);
        assert_eq!(i16::from_le_bytes([bytes[46], bytes[47]]), -32_767);
    }

    #[test]
    fn a_nonsense_sample_rate_from_the_engine_does_not_produce_a_broken_header() {
        for rate in [0, -1, i32::MIN] {
            let bytes = encode(&[0.0], rate);
            let written = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
            assert_eq!(written, FALLBACK_SAMPLE_RATE, "rate {rate}");
        }
    }

    #[test]
    fn what_we_encode_is_what_we_decode() {
        let original: Vec<f32> = (0..64).map(|i| (i as f32 / 32.0) - 1.0).collect();
        let clip = decode(&encode(&original, 16_000)).unwrap();
        assert_eq!(clip.sample_rate, 16_000);
        assert_eq!(clip.samples.len(), original.len());
        for (before, after) in original.iter().zip(clip.samples.iter()) {
            assert!((before - after).abs() < 1e-3, "{before} became {after}");
        }
    }
}
