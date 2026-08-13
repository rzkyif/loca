/**
 * Coordinates a captured speech segment through the transcription pipeline and
 * applies the result to the message input. Holds the transient processing /
 * autocorrect-diff state; the pure transcribe -> autocorrect -> merge chain
 * lives in lib/stt/transcription.
 *
 * Per the composable convention, the consumer owns lifecycle (it wires
 * `vadManager.attach(stt.handleVadAudio)` in onMount) and supplies the input
 * hooks (read/write text, focus, auto-send, notice) via the call context, so
 * this class never owns `text` or reaches into the streaming/send path.
 */

import { blobToBase64, float32ToWav } from "$lib/util/audio";
import { getLogger } from "$lib/util/log";
import { settingsState } from "$stores/settings.svelte";
import { defaultTranscriptionDeps, runTranscriptionChain } from "$lib/stt/transcription";

const sttLog = getLogger("stt");

export type SttContext = {
  getText: () => string;
  setText: (text: string) => void;
  focus: () => void;
  onAutoSend: () => Promise<void>;
  notice: (message: string) => void;
};

export class SttInput {
  // True while a captured segment is in the transcribe -> autocorrect -> merge
  // pipeline. The mic is already off by then (VAD one-shots on segment end), so
  // the placeholder reads this to stay on "Transcribing..." until it clears.
  processing = $state(false);
  // The autocorrect "Before" diff: the pre-correction text and whether to show
  // the revert affordance.
  showDiff = $state(false);
  original = $state<string | null>(null);

  /** Drop the autocorrect diff (on manual edit or send). */
  clearDiff(): void {
    this.showDiff = false;
    this.original = null;
  }

  async handleVadAudio(audio: Float32Array, ctx: SttContext): Promise<void> {
    this.clearDiff();
    this.processing = true;
    try {
      const wavBlob = float32ToWav(audio, 16000);
      const base64 = await blobToBase64(wavBlob);

      sttLog.info(`transcribing ${Math.round((audio.length / 16000) * 1000)}ms of audio`);
      const startedAt = performance.now();
      const result = await runTranscriptionChain(
        base64,
        ctx.getText(),
        {
          autocorrect: !!settingsState.currentSettings["stt.llmAutocorrect"],
          chain: !!settingsState.currentSettings["stt.llmChainTranscription"],
        },
        defaultTranscriptionDeps(),
        (stage, e) => sttLog.warn(`${stage} failed:`, e),
      );

      if (result.kind === "error") {
        sttLog.error("Transcription failed:", result.message);
        ctx.notice("Transcription failed! Please try again.");
        return;
      }
      if (result.kind === "empty") return;
      sttLog.info(`transcription pipeline done in ${Math.round(performance.now() - startedAt)}ms`);

      ctx.setText(result.text);
      this.original = result.original;
      this.showDiff = result.original !== null;
      ctx.focus();

      if (settingsState.currentSettings["stt.autoSend"]) {
        await ctx.onAutoSend();
      }
    } finally {
      this.processing = false;
    }
  }
}

export function useStt(): SttInput {
  return new SttInput();
}
