/**
 * The text-to-speech queue and player. Receives streaming assistant text,
 * batches it into chunks the synth handles efficiently, sends them to the
 * sidecar for synthesis, and chains the returned audio through an
 * HTMLAudioElement-based playback queue so audio starts as soon as
 * possible without gaps between chunks.
 */

import { browser } from "$app/environment";
import { cores } from "$lib/core";
import { getLogger } from "$lib/util/log";
import { stripEmojisForTTS, stripMarkdownForTTS } from "$lib/tts/text";
import { settingsState } from "./settings.svelte";

const log = getLogger("tts");

// Speech-sidecar load + readiness shims used by the queue below.

// How long to wait for the speech sidecar to come up before pre-warming, how
// long to keep waiting if it never even starts loading, and the poll interval.
const TTS_READY_TIMEOUT_MS = 20_000;
const TTS_READY_GRACE_MS = 3_000;
const TTS_READY_POLL_MS = 250;

const delay = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

async function loadTtsModel(): Promise<void> {
  // `/load` is a no-op: the core-managed speech sidecar owns the TTS model
  // lifecycle and spawns when tts.enabled flips on (services/sidecar-boot.ts).
  // Wait for it to report ready before returning, so the caller's pre-warm
  // synth lands on a loaded engine instead of racing the ~2 s sidecar spawn - a
  // race that 503s, fails the pre-warm silently, and leaves the user's first
  // real reply paying the cold-start cost.
  await cores().api().tts.load();
  const start = performance.now();
  let sawLoading = false;
  while (performance.now() - start < TTS_READY_TIMEOUT_MS) {
    let status: { loaded: boolean; loading: boolean } | null = null;
    try {
      status = await cores().api().tts.status();
    } catch {
      // Core momentarily unreachable (e.g. still booting); retry until the deadline.
    }
    if (status?.loaded) return;
    if (status?.loading) {
      sawLoading = true;
    } else if (!sawLoading && performance.now() - start > TTS_READY_GRACE_MS) {
      // Never began loading within the grace window: the sidecar isn't coming
      // up (most likely the model files aren't downloaded yet). Don't block the
      // full timeout; TTS loads lazily on the next synth once the files land.
      return;
    }
    await delay(TTS_READY_POLL_MS);
  }
}

async function synthesizeTts(text: string, voice?: string, speed?: number): Promise<Blob> {
  return await cores().api().tts.synthesize({ text, voice, speed });
}

const WORD_SEGMENTER = new Intl.Segmenter(undefined, { granularity: "word" });
function countWords(text: string): number {
  let n = 0;
  for (const seg of WORD_SEGMENTER.segment(text)) {
    if (seg.isWordLike) n++;
  }
  return n;
}

const SENTENCE_SEGMENTER = new Intl.Segmenter(undefined, {
  granularity: "sentence",
});

// One audio chunk in the playback queue. Holds the element, its owning blob
// URL (so we can revoke it after playback/reset), and the pre-measured
// effective duration (native duration adjusted for the element's playbackRate
// at scheduling time). Duration is captured up-front so the batcher's
// deadline estimator doesn't need to poll `audio.duration` at runtime.
type PlaybackEntry = {
  audio: HTMLAudioElement;
  url: string;
  effectiveDurationMs: number;
  /** Captured at schedule time and re-applied right before `play()` - some
   *  browsers reset the element's `playbackRate` to 1 when the source
   *  finishes loading, so assigning it earlier is effectively ignored. */
  playbackRate: number;
};

/** Split a pending blob into `head` (up to maxWords) and `tail` (remainder).
 *  Breaks on sentence boundaries so prosody stays clean. If the very first
 *  sentence already exceeds maxWords, it's taken whole - we'd rather send
 *  one oversized chunk than lose the sentence. */
function splitAtMaxWords(
  text: string,
  maxWords: number,
): { head: string; tail: string; headWords: number; tailWords: number } {
  const sentences = Array.from(SENTENCE_SEGMENTER.segment(text));
  let head = "";
  let headWords = 0;
  let i = 0;
  for (; i < sentences.length; i++) {
    const s = sentences[i].segment;
    const w = countWords(s);
    if (head && headWords + w > maxWords) break;
    head += s;
    headWords += w;
  }
  if (!head && sentences.length > 0) {
    head = sentences[0].segment;
    headWords = countWords(head);
    i = 1;
  }
  let tail = "";
  for (; i < sentences.length; i++) tail += sentences[i].segment;
  const tailWords = countWords(tail);
  return { head, tail, headWords, tailWords };
}

/** A short clip of 16-bit mono PCM silence wrapped in a WAV header. Used by
 *  the playback warm-up (see setEnabled) - it has to be a real playable
 *  source, but it must not make any noise. */
function makeSilentWav(durationMs: number): ArrayBuffer {
  const sampleRate = 24000;
  const dataSize = Math.round((sampleRate * durationMs) / 1000) * 2;
  const buf = new ArrayBuffer(44 + dataSize);
  const view = new DataView(buf);
  view.setUint32(0, 0x52494646, false); // "RIFF"
  view.setUint32(4, 36 + dataSize, true);
  view.setUint32(8, 0x57415645, false); // "WAVE"
  view.setUint32(12, 0x666d7420, false); // "fmt "
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  view.setUint32(36, 0x64617461, false); // "data"
  view.setUint32(40, dataSize, true);
  return buf;
}

// Smart-batching parameters. Kokoro is most efficient at ~100-200 tokens
// per request, which is roughly 25-50 words. We try to grow each chunk
// (after the first) up to MAX_WORDS, but never let a gap form between
// playback segments.
const MAX_WORDS = 50;
// Safety margin so we don't dispatch right when audio ends and risk a tiny gap
// from network/decode jitter. Covers the loadedmetadata wait, browser play()
// latency, and the audio-output device's prefill on chunk handoff.
const SAFETY_MS = 400;
// Floor for the re-check timer so we don't spin.
const MIN_RECHECK_MS = 75;

class TTSState {
  enabled = $state(false);
  loaded = $state(false);
  loading = $state(false);
  /** ID of the message TTS is currently voicing (streaming or replay).
   *  Cleared when playback fully drains or reset() is called. */
  currentMessageId = $state<string | null>(null);
  /** Number of audio sources currently queued/playing. Mirror of
   *  `liveSources.size`, exposed so components can react to "is audio
   *  actually coming out right now?" vs. "are we still synthesizing?". */
  liveSourceCount = $state(0);
  /** True while a /api/tts/synthesize request is in flight. Used with
   *  `liveSourceCount` to tell "loading" from "playing" in the UI. */
  synthInflight = $state(false);

  // Sentence-level pending buffer. Anything in here is text the streaming
  // detector has handed us but that hasn't been sent to /api/tts/synthesize.
  private pending = "";
  private pendingWords = 0;
  private streamFinalized = false;

  // We allow at most one in-flight synthesis at a time so that any text the
  // detector hands us while we wait can be batched into the next request.
  private inflight: Promise<void> | null = null;

  // HTMLAudioElement-based playback. We use the media element (rather than
  // an AudioBufferSourceNode) because `preservesPitch` lets us run
  // `tts.playbackSpeed` without chipmunking the voice. The trade-off is
  // coarser scheduling - chunks are chained via `onended` instead of at
  // the AudioContext sample clock, so there's a tiny browser-level gap
  // (~20-60 ms) between chunks.
  private playbackQueue: PlaybackEntry[] = [];
  private currentEntry: PlaybackEntry | null = null;
  // Wall-clock timestamp (performance.now ms) at which the current audio
  // started playing - used to compute remaining time for the batcher.
  private currentStartedAt = 0;

  // Learned synthesis cost in ms per word (EMA over observed chunks). Used
  // to predict how long the pending buffer will take to synthesize so we
  // can schedule dispatch to land exactly when the current playback ends.
  private msPerWord: number | null = null;

  // Re-check timer used while we're holding a pending batch back, waiting
  // for the playing chunk to wind down before dispatching the next one.
  private recheckTimer: ReturnType<typeof setTimeout> | null = null;

  // Every reset() bumps this. Each dispatch captures the current value; when
  // its synthesis resolves it compares against the epoch to detect "a reset
  // happened while I was in flight" - in that case the result is discarded
  // instead of being scheduled for playback.
  private resetEpoch = 0;

  async setEnabled(enabled: boolean): Promise<void> {
    if (!browser) return;
    this.enabled = enabled;
    if (enabled) {
      if (this.loaded || this.loading) return;
      this.loading = true;
      const t0 = performance.now();
      log.info("loading TTS model...");
      try {
        // Don't download here: flipping `tts.enabled` makes the core add the
        // Kokoro files to its required-files snapshot, which the single
        // requirements popup prompts for (user-confirmed). We just try to load;
        // if the files / `deno` worker aren't present yet, this fails
        // gracefully and TTS loads lazily on the next synth once they land.
        await loadTtsModel();
        // Pre-warm: the first /speak pays a one-time cost (the speech sidecar
        // loads the per-voice style vector and ORT does its JIT pass). Without
        // this the user's first synth chunk pays that cold-start, which shows
        // up as a noticeable gap before chunk 2 lands. Failure is non-fatal -
        // we still mark loaded so the user can use TTS.
        // Warm the playback side in parallel: the very first audio playback
        // of the process pays a one-time output-pipeline spin-up (WebKit
        // audio rendering + OS output device), during which the element's
        // clock advances while nothing is audible - the start of the first
        // real reply gets clipped. Absorb that cost with a silent clip now.
        const playbackWarm = this.warmPlayback();
        try {
          const settings = settingsState.currentSettings;
          const voice = (settings["tts.voice"] as string) || "af_bella";
          const synthSpeed = Math.max(
            0.25,
            Math.min(3, Number(settings["tts.synthesisSpeed"]) || 1),
          );
          await synthesizeTts("Hi.", voice, synthSpeed);
        } catch (e) {
          log.warn("pre-warm failed (non-fatal):", e);
        }
        await playbackWarm;
        this.loaded = true;
        log.info(`TTS ready in ${Math.round(performance.now() - t0)}ms`);
      } catch (e) {
        log.error("load failed:", e);
        this.loaded = false;
      } finally {
        this.loading = false;
      }
    } else {
      this.reset();
      this.loaded = false;
      // The speech sidecar owns TTS lifecycle (gated on tts.enabled), so this
      // is best-effort; disabling TTS in settings is what frees its model.
      try {
        await cores().api().tts.unload();
      } catch (e) {
        log.warn("unload failed:", e);
      }
    }
  }

  /** Play a short silent WAV at zero volume to spin up the audio output
   *  pipeline before the first real chunk needs it. Non-fatal on failure. */
  private async warmPlayback(): Promise<void> {
    const url = URL.createObjectURL(new Blob([makeSilentWav(300)], { type: "audio/wav" }));
    try {
      const audio = new Audio();
      audio.preload = "auto";
      audio.src = url;
      audio.volume = 0;
      await audio.play();
      await new Promise<void>((resolve) => {
        audio.onended = () => resolve();
        audio.onerror = () => resolve();
      });
    } catch (e) {
      log.warn("playback warm-up failed (non-fatal):", e);
    } finally {
      URL.revokeObjectURL(url);
    }
  }

  /** Reset any current playback / queue and claim the TTS pipeline for a new
   *  message. Keeps the learned msPerWord estimate across streams. */
  startStream(messageId: string | null = null): void {
    this.reset();
    this.currentMessageId = messageId;
  }

  /** Play an already-complete message's content through the batcher.
   *  Stops any current playback and clears the queue first. */
  replayMessage(messageId: string, content: string): void {
    if (!this.enabled || !this.loaded) return;
    this.reset();
    this.currentMessageId = messageId;
    let stripped = stripMarkdownForTTS(content);
    if (!settingsState.currentSettings["tts.spellOutEmojis"]) {
      stripped = stripEmojisForTTS(stripped);
    }
    if (!stripped) {
      this.currentMessageId = null;
      return;
    }
    const seg = new Intl.Segmenter(undefined, { granularity: "sentence" });
    for (const s of seg.segment(stripped)) {
      const chunk = s.segment.trim();
      if (chunk) this.feedSentence(chunk);
    }
    this.finalize();
  }

  /** Hand a completed (or near-completed on finalize) sentence to the batcher. */
  feedSentence(text: string): void {
    if (!this.enabled) return;
    if (!this.loaded) {
      // A boot-time load may have failed because the model files weren't
      // downloaded yet. Retry in the background (setEnabled no-ops while a
      // load is in flight) so TTS picks up on a later message without a
      // restart; the current sentence is dropped either way.
      if (!this.loading) void this.setEnabled(true);
      return;
    }
    const trimmed = text.trim();
    if (!trimmed) return;
    this.pending += this.pending ? " " + trimmed : trimmed;
    this.pendingWords += countWords(trimmed);
    this.maybeDispatch();
  }

  /** Mark stream as ended; flush whatever's still pending. */
  finalize(): void {
    if (!this.enabled || !this.loaded) return;
    this.streamFinalized = true;
    this.maybeDispatch();
  }

  private maybeDispatch(): void {
    if (this.inflight) return;
    if (this.pendingWords === 0) return;

    const settings = settingsState.currentSettings;
    const minWords = Math.max(1, Number(settings["tts.minChunkWords"]) || 8);

    let dispatch = false;
    let recheckIn = 0;

    if (this.streamFinalized) {
      // Stream done, no more text incoming - flush whatever we have.
      dispatch = true;
    } else if (this.pendingWords >= MAX_WORDS) {
      dispatch = true;
    } else {
      const playbackEndsInMs = this.playbackRemainingMs();
      if (playbackEndsInMs <= 0) {
        // First chunk of the stream (or playback already drained while we
        // waited for more text). Apply the low-latency rule from the user
        // setting so the first word reaches the listener fast.
        if (this.pendingWords >= minWords) dispatch = true;
      } else if (this.msPerWord !== null) {
        // We have an estimate. Dispatch when remaining playback time has
        // shrunk to "estimated synth time + safety". This lets the chunk
        // grow as long as possible while guaranteeing the result lands
        // before the speaker stops.
        const estProcessingMs = this.pendingWords * this.msPerWord + SAFETY_MS;
        if (playbackEndsInMs <= estProcessingMs) {
          dispatch = true;
        } else {
          recheckIn = playbackEndsInMs - estProcessingMs;
        }
      } else {
        // We haven't observed a synthesis yet but something is already
        // playing. Fall back to the min-words rule so we don't sit on text.
        if (this.pendingWords >= minWords) dispatch = true;
      }
    }

    if (dispatch) {
      this.dispatch();
    } else if (recheckIn > 0) {
      this.scheduleRecheck(recheckIn);
    }
  }

  private playbackRemainingMs(): number {
    let total = 0;
    if (this.currentEntry) {
      const elapsed = performance.now() - this.currentStartedAt;
      total += Math.max(0, this.currentEntry.effectiveDurationMs - elapsed);
    }
    for (const entry of this.playbackQueue) total += entry.effectiveDurationMs;
    return total;
  }

  private scheduleRecheck(delayMs: number): void {
    // Always reschedule with the freshest deadline. Each `feedSentence` grows
    // the pending buffer, which raises the estimated synth time and brings
    // the dispatch deadline closer; the previous timer would otherwise fire
    // late.
    if (this.recheckTimer != null) clearTimeout(this.recheckTimer);
    this.recheckTimer = setTimeout(
      () => {
        this.recheckTimer = null;
        this.maybeDispatch();
      },
      Math.max(MIN_RECHECK_MS, delayMs),
    );
  }

  private dispatch(): void {
    // Consume only up to MAX_WORDS of pending. Kokoro's tokenizer truncates
    // input beyond ~510 tokens, so if we shipped the entire buffer on a long
    // replay we'd silently drop the tail of the message. Any remaining text
    // stays in `pending` and the next `maybeDispatch` picks it up.
    const { head, tail, headWords, tailWords } = splitAtMaxWords(this.pending, MAX_WORDS);
    const text = head;
    const words = headWords;
    this.pending = tail;
    this.pendingWords = tailWords;
    if (this.recheckTimer != null) {
      clearTimeout(this.recheckTimer);
      this.recheckTimer = null;
    }

    const settings = settingsState.currentSettings;
    const voice = (settings["tts.voice"] as string) || "af_bella";
    // Synthesis speed is baked into the WAV Kokoro returns (shorter audio
    // for higher values, normal pitch). Playback speed is applied on the
    // AudioBufferSourceNode after decode (same pitch-shifted speed-up as
    // HTML5 `<audio>` playback rate). See scheduleAudio.
    const synthSpeed = Math.max(0.25, Math.min(3, Number(settings["tts.synthesisSpeed"]) || 1));

    const epochAtDispatch = this.resetEpoch;
    const t0 = performance.now();
    this.synthInflight = true;
    const synthPromise = synthesizeTts(text, voice, synthSpeed);

    this.inflight = (async () => {
      let wav: ArrayBuffer | null = null;
      try {
        const blob = await synthPromise;
        wav = await blob.arrayBuffer();
      } catch (e) {
        log.warn("synth failed:", e);
      }
      // If reset() fired while this synth was in flight, drop the result -
      // otherwise the user hears a chunk of audio pop in after they hit stop.
      if (epochAtDispatch !== this.resetEpoch) {
        this.inflight = null;
        this.synthInflight = false;
        return;
      }
      if (wav && wav.byteLength > 0) {
        // Update the learned synth cost. Slight bias toward the latest
        // observation so the estimate tracks model warm-up / cool-down.
        const synthMs = performance.now() - t0;
        log.debug(`synthesized ${words} words in ${Math.round(synthMs)}ms`);
        const observed = synthMs / Math.max(1, words);
        this.msPerWord = this.msPerWord === null ? observed : this.msPerWord * 0.4 + observed * 0.6;

        try {
          await this.scheduleAudio(wav, epochAtDispatch);
        } catch (e) {
          log.warn("decode/play failed:", e);
        }
      }
      this.inflight = null;
      this.synthInflight = false;
      // After dispatching, more text may have accumulated - re-evaluate.
      this.maybeDispatch();
    })();
  }

  private async scheduleAudio(wav: ArrayBuffer, epochAtDispatch: number): Promise<void> {
    const playbackRate = Math.max(
      0.25,
      Math.min(3, Number(settingsState.currentSettings["tts.playbackSpeed"]) || 1),
    );
    const url = URL.createObjectURL(new Blob([wav], { type: "audio/wav" }));
    const audio = new Audio();
    audio.preload = "auto";
    audio.src = url;

    // Wait for metadata so `audio.duration` is defined before we compute
    // the effective length.
    try {
      await new Promise<void>((resolve, reject) => {
        const onLoad = () => {
          audio.removeEventListener("loadedmetadata", onLoad);
          audio.removeEventListener("error", onErr);
          resolve();
        };
        const onErr = () => {
          audio.removeEventListener("loadedmetadata", onLoad);
          audio.removeEventListener("error", onErr);
          reject(new Error("audio metadata load failed"));
        };
        audio.addEventListener("loadedmetadata", onLoad);
        audio.addEventListener("error", onErr);
      });
    } catch (e) {
      URL.revokeObjectURL(url);
      throw e;
    }

    // The metadata wait above is a second async yield after dispatch()'s epoch
    // recheck, and this entry is not yet in currentEntry/playbackQueue, so a
    // reset() (stop / new message / navigate-away) landing during the wait can't
    // reach it. Re-check the epoch here and drop the chunk, or it audibly starts
    // playing after the user already stopped.
    if (epochAtDispatch !== this.resetEpoch) {
      URL.revokeObjectURL(url);
      return;
    }

    const effectiveDurationMs =
      Number.isFinite(audio.duration) && audio.duration > 0
        ? (audio.duration / playbackRate) * 1000
        : 0;

    const entry: PlaybackEntry = {
      audio,
      url,
      effectiveDurationMs,
      playbackRate,
    };
    audio.onended = () => this.onEntryEnded(entry);
    audio.onerror = () => this.onEntryEnded(entry);

    if (this.currentEntry) {
      // Something's already playing - queue this one for its turn.
      this.playbackQueue.push(entry);
    } else {
      // Nothing playing - start immediately.
      this.startEntry(entry);
    }
  }

  private startEntry(entry: PlaybackEntry): void {
    this.currentEntry = entry;
    this.currentStartedAt = performance.now();
    this.liveSourceCount = 1;
    // Apply preservesPitch + playbackRate right before play(). Some browsers
    // reset the element's playbackRate back to 1 when the source finishes
    // loading, so setting it earlier (e.g. right after `new Audio()`) gets
    // quietly ignored by the time the media is ready.
    const a = entry.audio as HTMLAudioElement & { preservesPitch?: boolean };
    a.preservesPitch = true;
    a.playbackRate = entry.playbackRate;
    const volRaw = Number(settingsState.currentSettings["tts.volume"]);
    const volPct = Number.isFinite(volRaw) ? volRaw : 100;
    a.volume = Math.max(0, Math.min(100, volPct)) / 100;
    entry.audio.play().catch((e) => {
      log.warn("play failed:", e);
      this.onEntryEnded(entry);
    });
  }

  private onEntryEnded(entry: PlaybackEntry): void {
    // Tear down this entry (even if it's mid-queue and got cancelled by
    // reset - in that case currentEntry/playbackQueue will already be
    // cleared, so we just free its URL).
    URL.revokeObjectURL(entry.url);
    entry.audio.onended = null;
    entry.audio.onerror = null;
    if (this.currentEntry !== entry) return;

    this.currentEntry = null;
    const next = this.playbackQueue.shift();
    if (next) {
      this.startEntry(next);
    } else {
      this.liveSourceCount = 0;
      if (!this.inflight && this.pendingWords === 0) {
        this.currentMessageId = null;
      }
    }
    this.maybeDispatch();
  }

  /** Stop any current playback and clear pending state. Keeps msPerWord. */
  reset(): void {
    this.resetEpoch++;
    if (this.recheckTimer != null) {
      clearTimeout(this.recheckTimer);
      this.recheckTimer = null;
    }
    this.pending = "";
    this.pendingWords = 0;
    this.streamFinalized = false;
    this.currentMessageId = null;

    const entries: PlaybackEntry[] = [];
    if (this.currentEntry) entries.push(this.currentEntry);
    entries.push(...this.playbackQueue);
    this.currentEntry = null;
    this.playbackQueue = [];
    for (const e of entries) {
      try {
        e.audio.onended = null;
        e.audio.onerror = null;
        e.audio.pause();
      } catch {
        /* ignore */
      }
      URL.revokeObjectURL(e.url);
    }

    this.liveSourceCount = 0;
    this.synthInflight = false;
  }
}

export const ttsState = new TTSState();

if (import.meta.hot) {
  // Dev-only. When HMR replaces this module the outgoing `ttsState` is orphaned
  // mid-playback: its current audio element keeps playing and its recheck timer
  // keeps firing maybeDispatch on a queue nothing feeds anymore. reset() stops
  // the audio, clears the queue, and cancels the timer so the swap is silent.
  import.meta.hot.dispose(() => ttsState.reset());
}
