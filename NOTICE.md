# Notice

Loca is licensed under [AGPL-3.0-only](LICENSE). It incorporates and/or
distributes the following third-party components. Each is licensed separately;
please consult the upstream project for its full license text.

## Binaries downloaded at runtime

`loca-core` fetches these compiled binaries from the Loca CDN on first use.
They are not part of this repository.

| Component                                           | Upstream                                | License |
| --------------------------------------------------- | --------------------------------------- | ------- |
| llama.cpp (`llama-server`)                          | <https://github.com/ggml-org/llama.cpp> | MIT     |
| ggml (shared libraries shipped with `llama-server`) | <https://github.com/ggml-org/ggml>      | MIT     |
| Deno runtime (`deno`)                               | <https://github.com/denoland/deno>      | MIT     |

## Linked into the core binary

| Component                                       | Upstream                                   | License          |
| ----------------------------------------------- | ------------------------------------------ | ---------------- |
| sherpa-onnx (in-process speech)                 | <https://github.com/k2-fsa/sherpa-onnx>    | Apache-2.0       |
| ONNX Runtime (statically linked into the above) | <https://github.com/microsoft/onnxruntime> | MIT              |
| espeak-ng + data (bundled in `loca-core`)       | <https://github.com/espeak-ng/espeak-ng>   | GPL-3.0-or-later |

`loca-core` statically links espeak-ng, which is licensed under
GPL-3.0-or-later. As required by that license, the corresponding source for
espeak-ng is available at <https://github.com/espeak-ng/espeak-ng>, at the
revision bundled by sherpa-onnx 1.13.3 (the version Loca builds against). Loca
does not modify espeak-ng.

LLM, speech-to-text, and text-to-speech model weights are also downloaded at
runtime from Hugging Face. Each model carries its own license, shown at download
time.

## Bundled with the desktop client

These ship inside the `loca-client` application bundle.

| Component                                     | Upstream                                   | License |
| --------------------------------------------- | ------------------------------------------ | ------- |
| Silero VAD v5 model (`silero_vad_v5.onnx`)    | <https://github.com/snakers4/silero-vad>   | MIT     |
| `@ricky0123/vad-web` (voice-activity worklet) | <https://github.com/ricky0123/vad>         | ISC     |
| ONNX Runtime Web (WebAssembly runtime)        | <https://github.com/microsoft/onnxruntime> | MIT     |

## Application dependencies

`loca-core` and `loca-client` link Rust crates; `loca-client` additionally
bundles npm packages. All are declared in the per-package `Cargo.toml` and
`deno.json` files. The most significant are:

| Component                    | License               |
| ---------------------------- | --------------------- |
| axum / tokio / reqwest       | MIT                   |
| Zod                          | MIT                   |
| Svelte / SvelteKit / Vite    | MIT                   |
| UnoCSS                       | MIT                   |
| Tauri (and official plugins) | MIT OR Apache-2.0     |
| `keyring` (Rust)             | MIT OR Apache-2.0     |
| DOMPurify                    | Apache-2.0 OR MPL-2.0 |
| highlight.js                 | BSD-3-Clause          |

For a complete, machine-readable license report, run `cargo about generate` for
the Rust dependency tree and a JSR/npm license tool over the `deno.json`
manifests.
