// Stages only the VAD assets the client actually loads into static/vad/.
//
// static/vad/ is gitignored and consumed by SvelteKit's adapter-static (which
// copies static/ -> build/, the dir Tauri ships as frontendDist). vad.svelte.ts
// calls MicVAD.new() with the default 'wasm' (CPU) execution provider, whose
// loader (ort-wasm-simd-threaded.mjs) fetches only ort-wasm-simd-threaded.wasm
// -- never the 25 MB WebGPU build ort-wasm-simd-threaded.jsep.wasm. Staging just
// the CPU files keeps that 25 MB out of every client bundle.
//
// onnxruntime-web is vad-web's own (transitive) dependency, so we locate it next
// to the resolved vad-web package rather than importing it directly: that pins
// the wasm + loader to the exact onnxruntime-web version vad-web loads, so the
// two can never drift (the latent bug the old hand-copied static/vad/ had).

import { emptyDir } from "@std/fs/empty-dir";
import { dirname, fromFileUrl, join } from "@std/path";

const clientRoot = dirname(dirname(fromFileUrl(import.meta.url)));
const vadOut = join(clientRoot, "static", "vad");

// vad-web has no exports map, so its dist/* subpaths resolve directly.
const worklet = fromFileUrl(
  import.meta.resolve("@ricky0123/vad-web/dist/vad.worklet.bundle.min.js"),
);
const model = fromFileUrl(import.meta.resolve("@ricky0123/vad-web/dist/silero_vad_v5.onnx"));

// onnxruntime-web sits beside vad-web in its package's node_modules
// (.../node_modules/{@ricky0123/vad-web, onnxruntime-web}); derive its dist/
// from vad-web's resolved path so we use the exact version vad-web pulled in.
const ortDist = join(dirname(worklet), "..", "..", "..", "onnxruntime-web", "dist");

const assets: ReadonlyArray<readonly [string, string]> = [
  [join(ortDist, "ort-wasm-simd-threaded.wasm"), "ort-wasm-simd-threaded.wasm"],
  [join(ortDist, "ort-wasm-simd-threaded.mjs"), "ort-wasm-simd-threaded.mjs"],
  [worklet, "vad.worklet.bundle.min.js"],
  [model, "silero_vad_v5.onnx"],
];

// emptyDir wipes any stale staging (including a previously copied 25 MB jsep
// wasm) so the directory ends up with exactly the files below.
await emptyDir(vadOut);
for (const [src, name] of assets) {
  await Deno.copyFile(src, join(vadOut, name));
  console.log(`staged vad/${name}`);
}
