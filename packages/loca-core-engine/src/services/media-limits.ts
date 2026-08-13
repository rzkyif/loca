// Shared input bounds for the speech modules, enforced at every entry point
// (the HTTP routes and the module broker's stdio ops) so no caller can stream
// an unbounded payload into memory or the synthesis engine.

/** Max characters per TTS synthesis call: synthesis time scales with length,
 *  and the audio payload is buffered whole before it is returned. */
export const TTS_MAX_TEXT_CHARS = 2_000;

/** Max bytes for a single STT audio upload: the file is read fully into memory
 *  (and base64-encoded across the broker's stdio pipe). */
export const STT_MAX_AUDIO_BYTES = 25 * 1024 * 1024;
