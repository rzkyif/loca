//! `SherpaSpeech`: the [`LocalSpeech`] implementation the routes and the
//! extension broker reach through.
//!
//! Three things in a line, and the order is the design: the fair-share
//! [`queue`](super::queue) first, so a saturated engine refuses immediately and
//! the permit covers the whole call; then the single
//! [`worker`](super::worker) thread holding that permit, with the decode INSIDE
//! the job. The decode is the expensive half (a 25 MB upload expands to roughly
//! 50 MB of `f32`), so doing it before the permit would put it outside the
//! queue's depth bound. The trade is that a malformed clip waits for a slot
//! before it is refused; the gain is that an abandoned one is never decoded.
//!
//! [`EngineOps`] keeps sherpa out of this file, so the bounds, the decode, the
//! queueing and the drop behaviour are all tested in the default build.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::{AppError, ErrorCode, Result};
use crate::speech::engine::{AudioClip, LocalSpeech, Synthesis};

use super::queue::Queue;
use super::wav;
use super::worker::Worker;

/// What the resident models can do, with no sherpa type in the signature.
///
/// Blocking by nature: every implementation runs on the speech worker thread.
pub trait EngineOps: Send + Sync {
    /// Mono samples in, transcript out.
    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String, String>;

    /// Text in, samples plus the engine's own sample rate out.
    fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: Option<f64>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(Vec<f32>, i32), String>;
}

/// Local speech over an in-process engine.
pub struct SherpaSpeech {
    queue: Arc<Queue>,
    worker: Arc<Worker>,
    engine: Arc<dyn EngineOps>,
}

impl SherpaSpeech {
    pub fn new(queue: Arc<Queue>, worker: Arc<Worker>, engine: Arc<dyn EngineOps>) -> Self {
        Self {
            queue,
            worker,
            engine,
        }
    }
}

#[async_trait]
impl LocalSpeech for SherpaSpeech {
    async fn transcribe(&self, audio: AudioClip, client_id: &str) -> Result<String> {
        // Decoded on the worker, under the permit: on a runtime thread the
        // expansion to roughly 50 MB of `f32` would sit outside the queue's
        // depth bound, so N simultaneous uploads would decode at once.
        let permit = self.queue.acquire(client_id).await?;
        let engine = Arc::clone(&self.engine);
        let cancelled = Arc::new(AtomicBool::new(false));
        let bytes = audio.bytes;
        let result = self
            .worker
            .run(cancelled, move || {
                let clip = wav::decode(&bytes)?;
                engine
                    .transcribe(&clip.samples, clip.sample_rate)
                    .map_err(|message| AppError::new(ErrorCode::ProviderError, message))
            })
            .await;
        // Explicit, so the release point is visible rather than implied by the
        // end of the function.
        drop(permit);
        result
    }

    async fn synthesize(&self, req: Synthesis, client_id: &str) -> Result<Vec<u8>> {
        let permit = self.queue.acquire(client_id).await?;
        let engine = Arc::clone(&self.engine);
        let cancelled = Arc::new(AtomicBool::new(false));
        let result = self
            .worker
            .run(Arc::clone(&cancelled), move || {
                engine
                    .synthesize(&req.text, req.voice.as_deref(), req.speed, cancelled)
                    .map_err(|message| AppError::new(ErrorCode::ProviderError, message))
            })
            .await;
        drop(permit);
        let (samples, sample_rate) = result?;
        Ok(wav::encode(&samples, sample_rate))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;
    use std::time::Duration;

    use crate::speech::local::queue::Unobserved;

    use super::*;

    /// One recorded synthesis call: the text, the voice selector, and the speed.
    type SpokenCall = (String, Option<String>, Option<f64>);

    #[derive(Default)]
    struct Fake {
        heard: Mutex<Vec<(usize, u32)>>,
        spoken: Mutex<Vec<SpokenCall>>,
        /// Blocks the engine until released, so a test can occupy the slot.
        gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        saw_cancel: Mutex<bool>,
        fail: Mutex<Option<String>>,
    }

    impl EngineOps for Fake {
        fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String, String> {
            self.heard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((samples.len(), sample_rate));
            if let Some(gate) = self.gate.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = gate.recv();
            }
            match self.fail.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                Some(message) => Err(message),
                None => Ok("a transcript".to_string()),
            }
        }

        fn synthesize(
            &self,
            text: &str,
            voice: Option<&str>,
            speed: Option<f64>,
            cancelled: Arc<AtomicBool>,
        ) -> Result<(Vec<f32>, i32), String> {
            self.spoken.lock().unwrap_or_else(|e| e.into_inner()).push((
                text.to_string(),
                voice.map(str::to_string),
                speed,
            ));
            if let Some(gate) = self.gate.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = gate.recv();
                // Poll the flag the way sherpa's generation callback does.
                for _ in 0..500 {
                    if cancelled.load(Ordering::Relaxed) {
                        *self.saw_cancel.lock().unwrap_or_else(|e| e.into_inner()) = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            Ok((vec![0.0, 0.5, -0.5], 24_000))
        }
    }

    fn speech() -> (SherpaSpeech, Arc<Fake>) {
        let fake = Arc::new(Fake::default());
        let speech = SherpaSpeech::new(
            Arc::new(Queue::new(Arc::new(Unobserved))),
            Arc::new(Worker::start()),
            Arc::clone(&fake) as Arc<dyn EngineOps>,
        );
        (speech, fake)
    }

    fn clip(samples: &[f32], rate: u32) -> AudioClip {
        AudioClip {
            bytes: wav::encode(samples, rate as i32),
            filename: "clip.wav".to_string(),
            mime: Some("audio/wav".to_string()),
        }
    }

    #[tokio::test]
    async fn a_clip_reaches_the_engine_as_samples_at_its_own_rate() {
        let (speech, fake) = speech();
        let text = speech
            .transcribe(clip(&[0.0, 0.25, -0.25, 0.5], 16_000), "client")
            .await
            .unwrap();
        assert_eq!(text, "a transcript");
        assert_eq!(
            fake.heard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_slice(),
            &[(4, 16_000)],
        );
    }

    #[tokio::test]
    async fn an_undecodable_clip_is_refused_without_reaching_the_engine() {
        // The containment property at this layer: bytes sherpa could choke on
        // never get there, and the caller is told why.
        let (speech, fake) = speech();
        let bad = AudioClip {
            bytes: b"this is not a wav".to_vec(),
            filename: "clip.wav".to_string(),
            mime: Some("audio/wav".to_string()),
        };
        let err = match speech.transcribe(bad, "client").await {
            Ok(_) => panic!("an undecodable clip reached the engine"),
            Err(err) => err,
        };
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(fake
            .heard
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty());
    }

    #[tokio::test]
    async fn synthesis_answers_wav_bytes_the_broker_can_read_the_rate_from() {
        let (speech, fake) = speech();
        let bytes = speech
            .synthesize(
                Synthesis {
                    text: "hello".to_string(),
                    voice: Some("af_bella".to_string()),
                    speed: Some(1.25),
                },
                "client",
            )
            .await
            .unwrap();

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            24_000,
        );
        let spoken = fake
            .spoken
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(
            spoken,
            vec![(
                "hello".to_string(),
                Some("af_bella".to_string()),
                Some(1.25)
            )],
        );
    }

    #[tokio::test]
    async fn an_engine_failure_becomes_a_provider_error_rather_than_a_panic() {
        let (speech, fake) = speech();
        *fake.fail.lock().unwrap_or_else(|e| e.into_inner()) =
            Some("local STT is not loaded".to_string());
        let err = match speech.transcribe(clip(&[0.0], 16_000), "client").await {
            Ok(_) => panic!("a failing engine reported success"),
            Err(err) => err,
        };
        assert_eq!(err.code, ErrorCode::ProviderError);
        assert_eq!(err.message, "local STT is not loaded");
    }

    #[tokio::test]
    async fn an_abandoned_synthesis_releases_the_slot_and_tells_the_engine_to_stop() {
        // Both halves of the drop obligation, since a fix for either alone
        // leaves the other broken.
        let (speech, fake) = speech();
        let (release, gate) = std::sync::mpsc::channel();
        *fake.gate.lock().unwrap_or_else(|e| e.into_inner()) = Some(gate);
        let speech = Arc::new(speech);

        let abandoned = {
            let speech = Arc::clone(&speech);
            tokio::spawn(async move {
                speech
                    .synthesize(
                        Synthesis {
                            text: "a long passage".to_string(),
                            voice: None,
                            speed: None,
                        },
                        "gone",
                    )
                    .await
                    .map(|_| ())
            })
        };
        // Wait until the engine is actually inside the call.
        for _ in 0..200 {
            if !fake
                .spoken
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        abandoned.abort();
        let _ = abandoned.await;
        // Let the engine reach its polling loop, then observe the flag.
        let _ = release.send(());

        // The next caller is served, which is the property that matters: the
        // slot was not held by work nobody is listening for.
        let next = tokio::time::timeout(
            Duration::from_secs(10),
            speech.transcribe(clip(&[0.0], 16_000), "next"),
        )
        .await
        .expect("the queue slot was never released by the abandoned caller");
        assert_eq!(next.unwrap(), "a transcript");
        assert!(
            *fake.saw_cancel.lock().unwrap_or_else(|e| e.into_inner()),
            "the engine was never told to stop generating",
        );
    }

    #[tokio::test]
    async fn callers_are_queued_rather_than_run_concurrently() {
        let (speech, fake) = speech();
        let (release, gate) = std::sync::mpsc::channel();
        *fake.gate.lock().unwrap_or_else(|e| e.into_inner()) = Some(gate);
        let speech = Arc::new(speech);

        let first = {
            let speech = Arc::clone(&speech);
            tokio::spawn(async move {
                speech
                    .transcribe(clip(&[0.0], 16_000), "a")
                    .await
                    .map(|_| ())
            })
        };
        for _ in 0..200 {
            if !fake
                .heard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let second = {
            let speech = Arc::clone(&speech);
            tokio::spawn(async move {
                speech
                    .transcribe(clip(&[0.0], 16_000), "b")
                    .await
                    .map(|_| ())
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            fake.heard.lock().unwrap_or_else(|e| e.into_inner()).len(),
            1,
            "the second caller entered the engine while the first was inside it",
        );

        let _ = release.send(());
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(
            fake.heard.lock().unwrap_or_else(|e| e.into_inner()).len(),
            2
        );
    }
}
