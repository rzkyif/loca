# External dependencies (break-glass reference)

Loca's model catalog and its manifest / update / binary-download paths depend
on third-party services and providers whose download URLs, API response shapes,
HTTP headers, and asset names are outside our control. When one of them changes,
a build or a user download breaks. This file is the map to fix it fast.

**Reach for this file when:** a model or sidecar download started failing, a
release build broke fetching an upstream asset, or `deno task test` flags a
schema / hash / size mismatch and you suspect a third party changed something.

Each entry is a pointer, not a re-explanation: it names the external contract we
rely on, the code that relies on it, the symptom when it breaks, and the fix.
The authoritative detail stays in the linked code and READMEs so this file
cannot drift from them.

Reading each row:

- **Assumes** is the exact external contract (URL shape, JSON field, header,
  asset name) we are betting will not change.
- **Fails as** is what you will actually observe when it does.
- **Fix** is the first move, plus the canonical doc to follow.

## 1. Model catalog (HuggingFace + scoring)

### HuggingFace resolve URL

- **Used for:** every model weight / mmproj / speech-model download.
- **Code:** [downloads/sources.rs](packages/loca-core-rs/src/downloads/sources.rs) builds the URL;
  [downloads/transfer.rs](packages/loca-core-rs/src/downloads/transfer.rs) runs the download.
- **Assumes:**
  `https://huggingface.co/{user}/{repo}/resolve/{branch}/{file}?download=true`
  serves the file (after a 302 to a CDN).
- **Fails as:** every catalog download 404s or returns HTML.
- **Fix:** update the template in `parseSource`. Specs are stored catalog-side
  as `@user/repo/branch/file`, so only the URL construction changes.

### HuggingFace redirect headers

- **Used for:** probing a download's size before fetching, and verifying its
  sha256 against HF + TLS.
- **Code:** [downloads/net.rs](packages/loca-core-rs/src/downloads/net.rs) reads `x-linked-size`
  (falling back to `content-length`) and `x-linked-etag` (falling back to
  `etag`) for the LFS content sha256.
- **Assumes:** the resolve endpoint 302-redirects and exposes `x-linked-size` /
  `x-linked-etag` on the HEAD response.
- **Fails as:** sizes show as 0 / unknown; the published-hash check silently
  yields nothing (catalog-pinned hashes still protect the download, see below).
- **Fix:** adjust the header names / redirect handling in those two spots.

### HuggingFace tree API (curation time)

- **Used for:** pinning each file's sha256 when curating the catalog.
- **Code:** `scripts/catalog/gather-hashes.ts`, `scripts/catalog/probe.ts`.
- **Assumes:**
  `https://huggingface.co/api/models/{repo}/tree/{branch}?recursive=true`
  returns `[{ path, lfs?: { oid } }]` with `oid` = the 64-hex git-LFS sha256.
- **Fails as:** `deno run -A scripts/catalog/gather-hashes.ts` errors or pins
  wrong / missing hashes.
- **Fix:** see the Content hashes section of the
  [model-catalog README](packages/loca-model-catalog/README.md); update the
  fetch + shape in `gather-hashes.ts`.

### GGUF header format (curation time)

- **Used for:** reading model architecture (block/head counts, context length)
  off the weights during curation.
- **Code:** `scripts/catalog/probe.ts`.
- **Assumes:** the `GGUF` magic and the `general.architecture` + `{arch}.*`
  metadata-key naming.
- **Fails as:** `probe.ts` throws "not a GGUF file" or reads `undefined`
  architecture fields.
- **Fix:** update the GGUF reader in `probe.ts` to the new metadata keys.

### Unsloth GGUF repos + quant naming

- **Used for:** which quants exist for each LLM family.
- **Code:**
  [packages/loca-model-catalog/src/families/](packages/loca-model-catalog/src/families/).
- **Assumes:** `@unsloth/<repo>/main/<Model>-<QUANT>.gguf` and `mmproj-F16.gguf`
  naming on the `main` branch.
- **Fails as:** a download 404s, or the file is missing at curation time.
- **Fix:** re-point the family file's specs, then regenerate hashes
  (`deno run -A scripts/catalog/gather-hashes.ts`) and re-release the catalog.

### csukuangfj sherpa-onnx STT/TTS repos + file roles

- **Used for:** speech-to-text and text-to-speech model files.
- **Code:** [stt.ts](packages/loca-model-catalog/src/stt.ts),
  [tts.ts](packages/loca-model-catalog/src/tts.ts).
- **Assumes:** per-family repo names and file roles (whisper:
  encoder/decoder/tokens; kokoro: model/voices/tokens; etc.) and the int8 / fp16
  / fp32 quant naming.
- **Fails as:** a speech-model download 404s or a required role file is absent.
- **Fix:** re-point the repo / file roles in `stt.ts` / `tts.ts`, regenerate
  hashes, re-release.

### Artificial Analysis intelligence-index scores

- **Used for:** ranking models in the fit engine (Smallest / Half / Full).
- **Code:** `packages/loca-model-catalog/src/families/*` (hand-entered values),
  `packages/loca-model-catalog/src/fit.ts` (the `intelligence-index` selector).
- **Assumes:** the `intelligence-index` metric keeps existing at
  <https://artificialanalysis.ai>. Values are hand-entered, not fetched at
  runtime, so nothing breaks live; the risk is stale or discontinued scores.
- **Fails as:** rankings drift from reality (no crash).
- **Fix:** re-verify scores on each curation pass; if the metric is renamed,
  switch the selector source/metric in `fit.ts` and across the family files.

## 2. Binaries / sidecars (GitHub releases)

### llama.cpp releases

- **Used for:** the `llama-server` sidecar (chat + embeddings).
- **Code:**
  [model.ts:101-129](packages/loca-shared/src/domain/model.ts#L101-L129)
  (`UPSTREAM_BINARIES`, the single source of truth for repo + per-triple asset
  patterns),
  [downloads/binaries/upstream.rs](packages/loca-core-rs/src/downloads/binaries/upstream.rs).
- **Assumes:** `api.github.com/repos/ggml-org/llama.cpp/releases/latest` returns
  `{ tag_name, assets[] }`, and the asset is named `llama-{tag}-bin-<platform>`
  per the `assets` map.
- **Fails as:** "upstream ... has no asset" on update / latest-channel resolve.
- **Fix:** update the asset-name patterns in `UPSTREAM_BINARIES`.

### deno (pinned v2.8.2)

- **Used for:** running tool workers under a pinned runtime.
- **Code:**
  [model.ts:113-128](packages/loca-shared/src/domain/model.ts#L113-L128).
- **Assumes:** `releases/tags/v2.8.2` exists with `deno-{triple}.zip` assets.
- **Fails as:** worker runtime download fails if the tag / assets vanish.
- **Fix:** bump `pinnedTag` deliberately, then **re-run the live-probe test**
  ([extensions/prompt/live_probe.rs](packages/loca-core-rs/src/extensions/prompt/live_probe.rs))
  because tool-permission prompts are parsed from this version's prompt wording
  ([extensions/prompt/parser.rs](packages/loca-core-rs/src/extensions/prompt/parser.rs)).

### GitHub release JSON shape + rate-limiting

- **Used for:** resolving every upstream sidecar (the two above).
- **Code:** [downloads/binaries/upstream.rs](packages/loca-core-rs/src/downloads/binaries/upstream.rs).
- **Assumes:** the `GitHubRelease` / `GitHubReleaseAsset` shape, and that
  `assets[].digest` is `sha256:...` (required to install on the latest channel).
  Unauthenticated GitHub is rate-limited (~60/hr); `GITHUB_TOKEN` raises it and
  a 5-minute per-repo cache dampens polling.
- **Fails as:** `manifest_fetch_failed` (rate limit / shape) or
  `checksum_mismatch` (missing digest).
- **Fix:** update the shapes / digest handling in `upstream.rs`; set
  `GITHUB_TOKEN` if rate-limited.

### espeak-ng-data (mutable rolling tag)

- **Used for:** the text-to-speech phonemizer data committed at
  `packages/loca-core-rs/data/espeak-ng-data.tar.gz` and embedded in the core.
- **Code:** `scripts/vendor-espeak.ts` (the pinned sha256).
- **Assumes:**
  `github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/espeak-ng-data.tar.bz2`
  stays reachable and matches the pinned sha256. The `tts-models` tag is
  **mutable**, so the asset can legitimately be republished.
- **Fails as:** `scripts/vendor-espeak.ts` aborts on a sha256 mismatch (by
  design: no silent propagation of a swapped asset).
- **Fix:** verify the new upstream asset, then recompute the pin and re-vendor
  the archive in the same commit.

### Archive formats

- **Used for:** extracting downloaded sidecars.
- **Code:** [downloads/binaries/extract.rs](packages/loca-core-rs/src/downloads/binaries/extract.rs).
- **Assumes:** `.tar.gz` (llama.cpp) and `.zip` (deno, Windows llama) layouts
  with the executable + shared libs.
- **Fails as:** extraction finds no executable / missing libs after a download.
- **Fix:** adjust the extraction layout in `extract.rs`.

## 3. Loca distribution (Cloudflare R2) + trust root

These are ours, not third-party, but the hostname / path layout and the signing
key are hard contracts between the release scripts and every installed core, so
they belong on the same map.

### R2 storage host + manifest paths

- **Used for:** fetching every signed manifest and release artifact.
- **Code:** [config.rs](packages/loca-core-rs/src/config.rs).
- **Assumes:**
  `get.loca.rzkyif.com/{manifests/<channel>/}{core,binaries,extension,catalog}.json`
  (stable is bare; latest/dev nest under the channel).
- **Fails as:** all update / catalog / extension fetches fail.
- **Fix:** all hardcoded hosts live in `config.rs`; changing them is a one-file
  edit. R2 / Cloudflare setup is in the
  [website README](packages/loca-website/README.md).

### Ed25519 trust root

- **Used for:** verifying every signed manifest and the model catalog.
- **Code:** public key in
  [data/signing-keys.json](packages/loca-core-rs/data/signing-keys.json), baked
  in by `build.rs` and verified in [update/manifest.rs](packages/loca-core-rs/src/update/manifest.rs),
  [downloads/binaries/manifest.rs](packages/loca-core-rs/src/downloads/binaries/manifest.rs),
  [extensions/install/manifest.rs](packages/loca-core-rs/src/extensions/install/manifest.rs), and
  [models/catalog.rs](packages/loca-core-rs/src/models/catalog.rs).
- **Assumes:** the committed public key matches the private key that signs
  releases; signatures cover the whole payload minus the `signature` field,
  canonicalized.
- **Fails as:** `signature_invalid` on update / catalog load across the board.
- **Fix:** the signing flow and trust root are documented in the
  [website README](packages/loca-website/README.md). Listed here only so the
  cross-cutting verification path is discoverable from one place.

### Android APK self-update (`android.json` + Java keystore)

- **Used for:** the Android client's self-update (the Tauri updater plugin has no
  Android support, so this path is bespoke).
- **Code:** manifest published by
  [release/android.ts](scripts/release/android.ts); verified + installed by
  [mobile.ts](packages/loca-client/src/ui/lib/platform/mobile.ts)
  (`checkAndroidUpdate`), which checks `android.json.sig` (Ed25519, same public
  key as above) and the per-ABI `sha256` before handing the APK to Android's
  package installer.
- **Assumes:**
  `get.loca.rzkyif.com/{manifests/<channel>/}android.json` (+ `.sig`) and the APK at
  `{channel-prefix}/v<ver>/<abi>/loca.apk`. The APK is signed by a Java keystore
  (base64 in `.env` as `LOCA_ANDROID_KEYSTORE_B64`); Android rejects an update
  not signed by the same key as the install, so a keystore change breaks
  upgrades for already-installed users.
- **Fails as:** the in-app updater silently returns "no update" (signature or
  hash mismatch), or Android refuses to install (keystore mismatch).
- **Fix:** keep the keystore stable across releases; the Android `versionCode`
  is Tauri-derived from `tauri.conf.json` `version`, so a same-version re-spin is
  NOT installable over the prior build without an uninstall (bump the version).
