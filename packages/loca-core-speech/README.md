# loca-core-speech

Standalone Rust crate compiled to its own small binary (not a `deno compile`
entry), so the binary boundary is explicit. It is the resident HTTP speech
sidecar for loca-core: one self-contained binary (statically-linked
sherpa-onnx) that serves speech-to-text and text-to-speech over a small loopback
HTTP API, across several sherpa-onnx model families (STT: whisper, sense-voice,
moonshine, paraformer, transducer, nemo-ctc, dolphin, telespeech; TTS: kokoro,
kitten, vits/piper, matcha). It replaces the earlier whisper-server sidecar and
the kokoro-js TTS worker. The header comment in [`src/main.rs`](src/main.rs) is
the canonical deep doc; this README stays short.

A single instance holds each engine in a slot that can be loaded or dropped at
runtime via `POST /configure`, so disabling TTS (or STT) frees its model while
the other stays resident, with no process restart.

## Distribution

Unlike the eager helper crates (which ship inside `core.json`), speech is
consent-gated and on-demand: built host-only during release, packaged with its
`espeak-ng-data`, and pinned into `binaries.json`, so it downloads only when
Speech-to-Text or Text-to-Speech is turned on. See
[`../../scripts/release/core.ts`](../../scripts/release/core.ts).

## Run, build, test

This crate exposes the standardized verbs as cargo wrappers (its `deno.json`):
`deno task lint:core-speech`, `build:core-speech`, or `cd` in and run
`deno task <verb>`. It is also folded into the repo-wide `deno task lint`. There
is no test suite, so `test:core-speech` just runs `cargo test` and finds
nothing.
