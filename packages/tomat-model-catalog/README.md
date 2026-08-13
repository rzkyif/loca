# @tomat/model-catalog

The hand-authored source for Tomat's local-model catalog. Tomat picks the three
adaptive presets (Smallest / Half / Full) and powers its model browser from this
data. Curation is centralized here so quality judgments stay with a human (or a
coding agent): things like "Gemma 4 QAT matches the standard model at lower
memory" cannot be inferred automatically.

## Layout

- `src/families/<family>.ts` - one file per model family. Adding a family is:
  write a file, add it to `src/families/index.ts`.
- `src/fit.ts` - the data-driven fit policy (budget fractions, the primary score
  selector, the Smallest quality floor, the tie-breaker order).
- `src/index.ts` - `buildCatalogPayload()` assembles + validates everything.

The schemas live in `@tomat/shared` (`domain/catalog.ts`): `ModelFamily`,
`CatalogModel`, `ModelArch`, `CatalogVariant`, `CatalogQuant`, `FitConfig`.

## Authoring a model

For each model record its GGUF variants, architecture, and capability score:

1. **GGUF variants + sizes** - from the provider's GGUF repo (currently
   Unsloth). Each `CatalogQuant` is `{ quant, modelSpec, fileSizeBytes }`, where
   `modelSpec` is the HF spec `@provider/repo/branch/file.gguf`. List quants
   best-quality first. A model may have multiple `variants` (e.g. `standard` and
   `QAT`); the QAT variant is tagged `["qat"]`.
2. **Architecture** (`arch`) - `blockCount`, `embeddingLength`, `headCount`,
   `headCountKv`, `headDim`. These drive the on-device KV-cache footprint math.
3. **Score** (`scores`) - a provider-tagged list; currently the Artificial
   Analysis Intelligence Index
   (`{ source: "artificial-analysis", metric:
"intelligence-index", value }`).
   QAT and standard share the model's score.
4. **Capabilities** - `tools`, `vision` (ships an mmproj), `reasoning`.

`scripts/catalog/probe.ts` gathers (1) and (2) from the HF API + GGUF headers:

```
deno run -A scripts/catalog/probe.ts unsloth/Qwen3.5-2B-GGUF [...repos]
```

Scores (3) come from <https://artificialanalysis.ai>; enter them by hand and
re-verify them on each curation pass.

## Content hashes

Every downloadable file is pinned to a content `sha256`, so a model download
verifies its bytes against the signed catalog instead of trusting HuggingFace's
`x-linked-etag` (which a proxy can strip or a non-HF mirror omit). The hashes
live in the generated `src/hashes.generated.ts` (`modelSpec -> sha256`), which
`buildCatalogPayload` stamps onto each quant / mmproj / speech file. After
adding or repointing any model, regenerate them:

```
deno run -A scripts/catalog/gather-hashes.ts
```

It reads the git-LFS `oid` from the HF tree API for weights (no download) and
hashes the few small non-LFS files (tokens, configs) directly. A spec left out
of the map is downloaded with a fallback to HF's published hash, so a missing
entry degrades gracefully rather than breaking the build.

## How the core consumes it

On-device, the core fetches `catalog.json` from
`get.au.tomat.ing/manifests/<channel>/catalog.json`, verifies its Ed25519
signature against the committed public key, caches it, and runs the LLM fit
engine (Smallest / Half / Full per device) locally. The Speech-to-Text catalog
skips fit entirely: whisper models fit everywhere. A dev-channel core builds the
payload directly from this in-repo source (no fetch, no signature), so catalog
edits show up in `deno task dev` immediately.

Because cards and model lists are data, re-pointing a preset or adding a model
is a catalog re-release: a new curation pass reaches users without a core or
client release. The reverse also holds: a core that changes the catalog schema
needs the catalog re-released with (or before) it.

## Build + release

- `deno task build:catalog` - validate the families and write an unsigned
  `dist/catalog.unsigned.json` for inspection. No keys, no upload.
- `deno task release` (or `release:stable`) - compiles, Ed25519-signs (same
  trust root as `core.json` / `binaries.json`), and uploads `catalog.json` to R2
  as part of the unified release whenever the catalog content changed. Bump the
  `version` in this package's `deno.json` when you change the families, or the
  release is rejected.
