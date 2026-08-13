# @loca/website

The Astro static site behind `loca.rzkyif.com`. Two pages:

- **Home** (`/`): a single-viewport feature **showcase** (animated, seekable
  previews of the app built from the real `@loca/shared/ui` components) above a
  compact install section.
- **User manual** (`/manual/...`): one MDX subsection per page, with a sidebar
  outline. Pages render the real `@loca/shared/ui` components inline as live
  demos. Each page's "Last updated" date is derived from the file's last git
  commit at build time (see `src/lib/git-date.ts`). How to write a page: the
  root [COPY.md](../../COPY.md) (its "User manual" section).

Release artifacts (installers, schemas, signed manifests, binaries) are **not**
served here; they live in the R2 public bucket at `get.loca.rzkyif.com`. The Worker
serves only the two pages above as static assets.

## Develop

```sh
deno task dev:website       # Astro dev server (http://localhost:4321)
deno task build:website     # astro build -> packages/loca-website/dist/
```

For a Workers-runtime preview (after a full `deno task build`):

```sh
cd packages/loca-website && deno task preview    # wrangler dev
```

## Structure

- `src/pages/` -- `index.astro` (home), `install.astro` (the install page), and
  `manual/[...slug].astro` (one page per manual entry).
- `src/components/showcase/` -- the showcase: `Showcase.svelte` (snap-scroll
  track + scrub bar + auto-advance) drives per-feature stages (`ChatStage`,
  `SettingsStage`) that script a GSAP timeline over the shared view components.
- `src/components/` -- `Navbar`, `ManualOutline`, `ThemeToggle`, the install
  page's `InstallDiagram` + `InstallGenerator` (`.astro` no-JS baseline +
  `.svelte` enhanced island), and the small static `demos/` reused inside manual
  pages.
- `src/content/manual/` -- the manual markdown, grouped into sections.
- `src/lib/` -- `site.ts` (nav config), `manual.ts` (section grouping),
  `showcase.ts` (the GSAP cursor / typing / scroll helpers).
- The app look comes entirely from `@loca/shared/ui` (components + `base.css` +
  the UnoCSS preset); the site adds only layout and the showcase choreography.

## Release & deploy

The site is a platform-independent item of the umbrella release, alongside the
model catalog and schemas: it carries no per-triple artifact, so it
builds + deploys directly on the release host with no build-matrix runner. From
the **repo root** `deno task release` (or a remote-triggered channel push) runs
`astro build` then `wrangler deploy` for it, gated by the same source-hash cursor
on R2 and version-bump check every other item uses, so an unchanged site is a
no-op and no other artifact is built when only the site changed. The website's
version lives in `packages/loca-website/deno.json`; for the full version-bump
table and channel details see [DEVELOPMENT.md](../../DEVELOPMENT.md#channels).

To ship a website-only change through CI: push it to `main`, then
`deno task remote-release`. Preflight sees only the landing page changed
(`native=false`), skips the entire build matrix, and the publish job deploys just
the site.

**One-time Cloudflare setup** (needs the `loca.ing` zone on Cloudflare):

```sh
deno run -A npm:wrangler@^4 r2 bucket create loca-releases    # release artifacts
```

wrangler authenticates with `CLOUDFLARE_API_TOKEN` (+ `CLOUDFLARE_ACCOUNT_ID`)
from the repo-root `.env`, the same credentials the rest of the release uses; if
both are blank it falls back to a stored `wrangler login` OAuth session. Then in
the Cloudflare dashboard attach the custom domains: `get.loca.rzkyif.com` to the R2
bucket (R2 -> loca-releases -> Settings -> Custom Domains; enables public read)
and `loca.rzkyif.com` to the Worker (set on first `deploy` via `wrangler.toml`).
Seed `.env` at the repo root from `.env.example` (release-only: manifest signing
plus Cloudflare/R2 credentials; the signing keypair is generated on first run).

## GitHub Actions release (branch-driven)

Besides the local `deno task release`, a second pipeline releases each channel by
a git transfer (`.github/workflows/release.yml`):

- fast-forward `main` -> `latest` publishes the **latest** channel;
- fast-forward `latest` -> `stable` publishes the **stable** channel.

Drive it from the repo root without touching your working tree:

```sh
deno task remote-release          # remote latest <- main   -> CI publishes latest
deno task remote-release:stable   # remote latest <- stable -> CI publishes stable
```

`remote-release` is purely remote: it fast-forwards the target branch onto its
source via the GitHub refs API (`gh`), refuses a non-fast-forward, and no-ops
when the target is already aligned (so it never triggers a redundant run). It
replaces the manual `git push origin main:latest`.

**The channel branches are shared with the local pipeline.** A `preflight` job
diffs the release items against the shared R2 cursor and emits two gates:
`changed` (any item differs -> run the publish job) and `native` (a matrix-built
item - core, desktop client, android - differs -> run the expensive per-triple
build matrix). So `deno task release` (which publishes to R2 locally first, then
fast-forwards and pushes the channel branch) lands a branch push that preflight
sees as "nothing changed" -> every job is skipped. `remote-release` moves the
branch with nothing published yet -> preflight sees changes -> CI does the work.
A push that touches only platform-independent items (extension, catalog, install
scripts, schemas, the landing page) has `native=false`, so the whole build matrix
is skipped and the publish job builds + deploys just those items on its own. The
two pipelines never double-publish, and neither wastes a matrix build.

The channel is the branch name. The workflow fans out one native build runner per
desktop triple (`macos-13` x64, `macos-14` arm64, `windows-latest` x64,
`windows-11-arm` arm64, `ubuntu-latest` x64 + Android), each running
`deno task release:ci-build` to build and build-time-sign its own triple (Tauri
minisign for the desktop bundle, the Java keystore for the APK) and upload a
staging artifact. A single coordinator then runs `deno task release:ci-publish`:
it reconstructs `dist/`, composes + Ed25519-signs + uploads the unified manifests
**once** to R2, and mirrors the run to a rolling per-channel **GitHub Release**
(tags `latest` / `stable`, assets clobbered each run).

This is the same compose/sign/upload code and the **same R2 release-state cursor**
as the local pipeline, so a CI run and a local run can never double-publish; the
CI build runners are just an alternative environment provider to the local
Podman/UTM drivers, producing the same bundle/descriptor contract
(`scripts/release/artifacts.ts`). The Ed25519 trust-root **private** key is given
only to the publish job, never to a build runner (which gets only the public key,
baked into the core binary, plus the Tauri/Android build-signing keys).

**Version bumps live on `main`.** The channel branches stay clean fast-forwards
and CI never writes the repo: a changed item that wasn't bumped past the published
version fails the publish job loudly (`--noBump` records the as-built source hash,
so re-pushing the same commit is a no-op). Bump versions as part of the work on
`main` before transferring (see the version-bump table in
[DEVELOPMENT.md](../../DEVELOPMENT.md#channels)).

**Required repo secrets** (same names as `.env.example`): `LOCA_SIGNING_PRIVATE_KEY_B64`,
`LOCA_SIGNING_PUBLIC_KEY_B64`, `TAURI_UPDATER_PUBLIC_KEY`, `TAURI_UPDATER_PRIVATE_KEY`,
`TAURI_UPDATER_PRIVATE_KEY_PASSWORD`, `LOCA_ANDROID_KEYSTORE_B64`,
`LOCA_ANDROID_KEYSTORE_PASSWORD`, `LOCA_ANDROID_KEY_ALIAS`, `LOCA_ANDROID_KEY_PASSWORD`,
`CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_API_TOKEN`, `LOCA_R2_BUCKET`, `LOCA_STORAGE_DOMAIN`.
Optionally, the Apple signing vars, which cover the macOS Developer ID bundle,
the macOS `.pkg` installer, and the iOS App Store build (`APPLE_SIGNING_IDENTITY`,
`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
`APPLE_INSTALLER_SIGNING_IDENTITY`, `APPLE_INSTALLER_CERTIFICATE`,
`APPLE_INSTALLER_CERTIFICATE_PASSWORD`, `APPLE_TEAM_ID`, and the
`APPLE_API_KEY` / `APPLE_API_ISSUER` / `APPLE_API_KEY_B64` App Store Connect
trio - the `.p8` as base64, since a runner cannot carry a file path; local
`.env` uses `APPLE_API_KEY_PATH` instead) - these belong on the **macOS build
runner only** (build-time signing, like the Tauri/Android keys), never the
publish job. Purely secret-driven, every var optional: when unset the macOS
bundle stays ad-hoc-signed, the `.pkg` ships unsigned, and the iOS `.ipa`
build/upload is skipped with a note; when set, the same runs sign + notarize
the client bundle and the unified `.pkg` and ship iOS, with no other switch.
Optionally, the **dormant** Windows Authenticode vars
(`WINDOWS_CERTIFICATE_THUMBPRINT` or `WINDOWS_SIGN_COMMAND`, with
`WINDOWS_TIMESTAMP_URL`) sign the client NSIS + the unified `.exe` installer on
the **Windows build runner only**; unset today, so those installers ship
unsigned.
iOS is distributed through the App Store, not R2, so its runner builds +
uploads the `.ipa` to App Store Connect itself rather than staging for the
publish coordinator. See
[macos-signing.md](../../scripts/release/macos-signing.md).
Build runners receive only the Tauri + Android (+ Apple, on the mac runners)
subset (plus the signing public key); the publish job receives all. The landing
page is a platform-independent item built by the publish coordinator, so a
website-only change publishes through these workflows with the whole build matrix
skipped (preflight's `native` gate is false).
