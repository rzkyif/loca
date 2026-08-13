// Tier 1: speech-to-text. Exercises the real client -> core transcription path
// (the loca-owned part of STT) against the external STT mock, which returns a
// deterministic transcript so the test is reliable.
//
// Note on simulating speech: the mic-capture + voice-activity detection is
// @ricky0123/vad-web (Silero), a third-party lib, not Loca code. Driving Silero
// from synthetic audio is unreliable; the deterministic seam Loca owns is the
// transcription request, which we drive here with canned WAV bytes (the mock
// ignores the audio and returns a fixed transcript). To also exercise VAD end to
// end, feed a real speech WAV via Chromium's --use-file-for-fake-audio-capture.
import { afterEach, expect, test } from "vitest";
import { launchApp, type AppHandle } from "../../harness/app.ts";
import { cores } from "@client/lib/core/cores.ts";
import { defaultTranscriptionDeps, runTranscriptionChain } from "@client/lib/stt/transcription.ts";

let app: AppHandle | undefined;
afterEach(async () => {
  await app?.dispose();
  app = undefined;
});

// A tiny valid WAV; the external STT mock ignores the bytes.
function makeWavBlob(): Blob {
  const sampleRate = 16000;
  const samples = 1600;
  const buf = new ArrayBuffer(44 + samples * 2);
  const view = new DataView(buf);
  const w = (off: number, s: string) => {
    for (let i = 0; i < s.length; i++) view.setUint8(off + i, s.charCodeAt(i));
  };
  w(0, "RIFF");
  view.setUint32(4, 36 + samples * 2, true);
  w(8, "WAVE");
  w(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  w(36, "data");
  view.setUint32(40, samples * 2, true);
  return new Blob([buf], { type: "audio/wav" });
}

async function blobToBase64(blob: Blob): Promise<string> {
  const buf = new Uint8Array(await blob.arrayBuffer());
  let bin = "";
  for (const b of buf) bin += String.fromCharCode(b);
  return btoa(bin);
}

test("transcribes audio through the real client SttApi", async () => {
  app = await launchApp({ scenario: "paired", settings: { "stt.enabled": true } });
  await app.chat.waitReady();
  const res = await cores().api().stt.transcribe(makeWavBlob(), undefined);
  expect(res.text).toContain("hello from speech");
});

test("runs the client transcription chain end to end", async () => {
  app = await launchApp({ scenario: "paired", settings: { "stt.enabled": true } });
  await app.chat.waitReady();
  const audioBase64 = await blobToBase64(makeWavBlob());
  const result = await runTranscriptionChain(
    audioBase64,
    "",
    { autocorrect: false, chain: false },
    defaultTranscriptionDeps(),
    () => {},
  );
  expect(result.kind).toBe("ok");
  if (result.kind === "ok") expect(result.text).toContain("hello from speech");
});
