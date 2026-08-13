# AGENTS.md (website)

Scoped guidance for `@loca/website`. The repo-wide [AGENTS.md](../../AGENTS.md)
still applies; this file adds what is specific to the site. Keep it at altitude:
conventions and decisions, not command lists (those live in
[README.md](README.md)).

## Overview

A thin Astro static site behind `loca.rzkyif.com` with two pages: a single-viewport
feature **showcase** home and a markdown **user manual**. It renders the real
`@loca/shared/ui` components so the previews match the app exactly; the site
itself owns only layout and the showcase choreography. Release artifacts live on
R2 (`get.loca.rzkyif.com`), never here.

## Conventions

- **Exact-parity rule (CRITICAL, no exceptions).** Any loca-client component
  represented on the site must render EXACTLY as the app does on default
  settings: spacing, colors, rounding, hover, fonts, transitions. The only way
  to show one is to render the shared `@loca/shared/ui` component, never a
  re-implementation.
  - **Single source means BOTH sides render it.** A shared component is only
    legitimate if the CLIENT also wraps it. A `*View` that only the website uses
    is a re-implementation in disguise and WILL drift (the settings demo did
    exactly this: a website-only `SettingsContentView` silently lost the group
    header and the sidebar footer because the client never rendered it). Before
    adding or trusting any shared component for a preview, confirm the client
    wraps the SAME component.
  - **Every layer, not just primitives.** Composition is a component too: the
    field row, the section, the group header, the sidebar, the whole panel must
    each be a single shared component that both sides render. Sharing only the
    leaf primitives (Toggle, Input) while each side hand-assembles them is NOT
    enough - the assembly drifts.
  - **Extend, never fork.** If the site needs something a shared component does
    not do yet, extend the shared component (inject client-only behavior -
    validation, pickers, capture, live catalogs - via props/callbacks/snippets)
    so the client keeps using it. Never fork its markup onto the site.
  - **Breakage direction.** If extracting a layer into shared breaks the client,
    fixing the client to wrap the shared component is the next task. A
    client-vs-website divergence is never an acceptable resting state.
- **Sample data comes from `@loca/shared/ui/samples`.** Every `*View` has named
  sample prop bundles there (built from the settings schema, never hardcoded);
  the showcase stages, the demos, and the component **gallery** all consume them
  so demo state has one home. The gallery (`src/pages/gallery.astro` +
  `components/gallery/Gallery.svelte`) is a **masonry** of cards rendering every
  shared View and primitive from its samples, each on the background it ships on
  (chat / settings / a dimmed modal backdrop), with no `UiContext` provider (so
  it falls back to `DEFAULT_UI_CONTEXT`, the desktop shell); it is the canonical
  parity/QA surface and the manual's screenshot source. What it must cover is the
  typed `components/gallery/registry.ts` (`GALLERY_VIEWS` + `GALLERY_PRIMITIVES`),
  which the renderer iterates and the lint walkers parse. The mobile counterparts
  live in a separate `components/gallery/MobileGallery.svelte` section that scopes
  a `platform: "mobile"` provider to its own subtree (so the desktop cards stay
  on `DEFAULT_UI_CONTEXT`) and frames each card in a phone-sized viewport, so the
  touch branches (bottom-sheet modals, the stacked settings nav, the fullscreen
  chat shell) get visual coverage too. The lint walkers mechanically enforce that
  every View has a sample + a registry entry + a client wrapper, every primitive
  has a sample + a registry entry, and no client shell carries un-extracted raw
  styled markup (so a website-only View, an uncovered primitive, or bespoke
  client markup that never reaches the gallery is illegal).
- **Demo-driven interactive states use `hov:` / `act:`.** The scripted cursor
  sets `data-hover` / `data-active` on its target; the shared `hov:`/`act:`
  UnoCSS variants resolve those to the same styles as `:hover`/`:active`, so a
  demo and a real pointer paint identically with no duplicated CSS. Interactive
  shared components already use these; new ones should too.
- **Showcase animation.** Timelines are GSAP, built `paused` and driven by the
  `Showcase` container (auto-play when not scrubbed/scrolling, snap-advance on
  complete). Motion that mirrors a real app transition must use the shared
  helpers (`slideSwap`, `collapseLabel`, `runExpand`) and the canonical timing
  (`BASE_MS`, `CSS_EASING`) so it matches the app's numbers; cursor moves and
  typing are demo-authored. Target elements by stable selectors (`aria-label`,
  `title`, or `data-demo`), resolved lazily so mid-timeline elements still bind.
- Inherited from the repo: no em dashes, the brand written **Loca**, prefer
  maintained packages.

## Key Decisions

- **No-JS support is dropped, except the install page.** Build features the best
  way; priority order is (1) exact component parity, (2) mobile, (3) general best
  practices. JS-only interactivity (the showcase) is fine. The one carve-out is
  `/install`: installing must never be JS-gated, so it ships a native-HTML
  baseline (`InstallGenerator.astro`: `<input type=radio>` + CSS `:has()` drive
  the OS selection and reveal the pre-rendered installer downloads + how-to; the
  shared primitives are JS-driven, so the baseline cannot use them) and
  progressively enhances with a Svelte island (`InstallGenerator.svelte`: OS +
  arch auto-detect with a recommended-download highlight). The two are swapped
  by a `data-js` gate in `site.css`. The topology diagram
  (`InstallDiagram.astro`) is pure SVG + SMIL, so it animates with no JS too.
- **The home page is one `100dvh` viewport:** the showcase (most of the space)
  above a compact install call-to-action, capped to the content width, with no
  vertical page scroll.
- **Three pages.** Home, Install, and the User Manual. The changelog was removed;
  releases are tracked elsewhere.
- **Last updated is git-derived.** Each manual page's "Last updated" date comes
  from the file's last git commit at build time (`src/lib/git-date.ts`), not
  from frontmatter.

## Pointers

- Shared UI system (the source of truth for every preview):
  [`packages/loca-shared/src/ui/README.md`](../loca-shared/src/ui/README.md)
  (pattern, tiers, the `*View` contract, samples, gallery, enforcement), over
  `packages/loca-shared/src/ui` (components, `context.ts`, `samples/`,
  `animations.ts`, `uno-preset.ts`, `styles/base.css`).
- Settings schema + defaults: `packages/loca-shared/src/domain/settings`
  (`getDefaultSettings`).
- Writing manual copy (voice, terminology, page shape, the inline-demo rule):
  the root [COPY.md](../../COPY.md).
- Run / build / release / Cloudflare setup: [README.md](README.md).
- Repo-wide dev loop + channels: [DEVELOPMENT.md](../../DEVELOPMENT.md).
- Content-lock tooling: `scripts/website/`.
