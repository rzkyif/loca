# Shared UI (`@loca/shared/ui`)

The presentational layer both the client and the website render, so a preview on
the site is the same component the app ships. A future Tauri-mobile build reuses
the same layer unchanged. This file is the canonical reference for how that
layer is structured; the package [README](../../README.md), the
[website AGENTS.md](../../../loca-website/AGENTS.md), and the
[client README](../../../loca-client/README.md) point here.

## The pattern: presentational backbone + narrow provider

A shared component is **presentational**: props in, callbacks out, snippets for
caller-injected sub-trees. The client wraps it feeding live store state; the
website (and the gallery) wrap it feeding sample state. A truly reusable
component has no expectations from its host, so every integration arrives by
prop/callback/snippet, never by reaching into client code.

The one exception is **ambient, cross-cutting state** that would otherwise
prop-drill through many unrelated layers. That goes through the `UiContext`
provider ([context.ts](context.ts)) as dependency injection, not state
management. Members today: alignment, bubble-blur, the animation-duration
resolver, the expansion registry, system-message color, and the host facts
`platform` / `density` / `pointer`. When no provider is mounted (the website
gallery, a standalone preview), `DEFAULT_UI_CONTEXT` supplies schema defaults so
the render matches a fresh app.

Every `UiContext` is built by the **`makeUiContext(sources)`** factory, never by
hand. The settings-derived members (alignment, blur, the system-message tint -
including the "transparent value means no override" rule) are computed once in
the factory, so the three call sites (the client provider in `+layout.svelte`,
`DEFAULT_UI_CONTEXT`, and the website showcase) cannot drift. A caller supplies
only `getSetting` plus the host behaviors that genuinely differ
(`animationDurationMs`, the `expansion*` registry hooks); everything else falls
back to a presentational default.

**Expansion** (collapsible bubbles) lives in the context's per-id registry as
the single source of truth. A View reads/writes it with a Svelte function
binding
(`bind:expanded={() => ui.expansionGet(id, def), (v) => ui.expansionSet(id, v)}`)
rather than a local mirror, and calls `ui.expansionInit(id, def)` once on mount
so a default-expanded bubble seeds the registry the message-stack layout reads.

Decision rule, context vs prop: use `UiContext` only if the value is ambient,
read by many Views across unrelated subtrees, and would otherwise drill through
layers that do not care about it. Everything per-instance is a prop.

## Component tiers

Every UI component is one of four tiers. The client manifest
(`packages/loca-client/src/ui/components/.tiers.json`) records each client
component's tier; the `check-component-tiers` walker keeps it honest.

- **A0 - Shared primitive** (`components/<Name>.svelte`, no `View` suffix): a
  leaf with no domain knowledge (Button, Input, Toggle, Bubble, Modal, ...).
- **A - Shared View** (`components/<Name>View.svelte`): a composed, domain-
  _shaped_ piece of UI that never fetches or mutates. Reads ambient state only
  via `useUiContext()`. Must have a sample and a gallery card.
- **B - Thin client container** (`<Name>.svelte` wrapping one `<Name>View`):
  logic + `<NameView ... />` + injected snippets; no markup of its own.
  Canonical example: `chat/SessionBar.svelte`.
- **C - Client-owned shell** (no single matching View): orchestrates lifecycle /
  multiple stores / browser APIs, or composes several Views. Every _visible_
  leaf inside it must be a Tier A View or A0 primitive (the `check-component-tiers`
  raw-leaf cap enforces this); any raw styled leaf is declared as
  `unsharedLeaves`. It may optionally list the galleried Views it renders in
  `composes` (and child client components in `orchestratorOf`) to document its
  coverage. Example: `chat/UserInput.svelte`.

## The `*View` contract

Order `$props()` as: (1) data props (plain values/objects, never a store or a
`*State` instance); (2) `on*` callbacks (optional, with a `noop`/guarded default
so a no-handler render does not crash); (3) snippet props (render a static
stand-in when absent, as `SettingsFieldView`'s `complexField` does); (4)
`$bindable()` only for parent-owned controlled inputs.

Forbidden inside any A0/A component (enforced by `check-shared-ui-purity`):
imports from `@loca/client` / `$lib` / `$stores` / `$composables`,
`@tauri-apps/*`, `@loca/shared/api/*`, and any lifecycle side-effect beyond
rendering. A local presentational `$effect` on local `$state` is fine.

## Samples + gallery

[`samples/`](samples/) exports named sample prop bundles for each View, typed
`satisfies Record<string, OmitSnippetProps<ComponentProps<typeof FooView>>>` so
a prop rename OR a dropped required (non-snippet) prop fails `svelte-check`.
Snippet props are excluded ([`samples/types.ts`](samples/types.ts)) because the
renderer supplies them. Domain values come from `getDefaultSettings()` /
`SETTINGS_SCHEMA`, never hardcoded, so a sample equals a fresh app. Snippet
props are supplied by the renderer (snippets cannot live in a `.ts` file). The
`SAMPLES` registry in [`samples/index.ts`](samples/index.ts) is keyed by View
name for the coverage walker.

Primitives have their own sample bundles in
[`samples/primitives.ts`](samples/primitives.ts) (`PRIMITIVE_SAMPLES`, keyed by
component name, typed `Partial<ComponentProps<...>>` so the renderer can inject
callbacks/children). They live apart from `SAMPLES` so the `endsWith("View")`
logic stays clean.

The website **gallery** (`packages/loca-website/src/pages/gallery.astro` +
`components/gallery/Gallery.svelte`) is a **masonry** of cards, each rendering a
component from its sample over the same dim **focus grid** the homepage showcases
use (`bg-surface` + `.focus-grid-frame`, see `GalleryCard.svelte`), with no
`UiContext` provider mounted. Like the manual demo frames each card is a DOUBLE
theme flip: `.demo-frame` renders the card chrome + grid in the opposite site
theme, and a `.demo-unflip` wrapper restores the website theme for the rendered
component, so a dark site shows a light card framing a dark component (and vice
versa). Because of that flip a component that ships on top of a bubble must sit on
a `bg-surface` panel or its dark text lands on the opposite-theme card chrome; so
settings fields, the object scaffolding, and every primitive render inside the
card's `surface` panel, while chat-message Views and modals (which carry their own
surface) sit straight on the grid. Wide shells (and the wider chat pieces: the composer, the
bars, the session list, the message stack) take a full-width row of their own
instead of a narrow masonry column, where they keep the faithful 700px-window
proportions; narrow tiles re-cap their component to the card width (so the bubble
background and its drop shadow stay in sync). The ChatShellView card renders the
real chat on a short simulated session, and every card is addressable: its label
(only the label, never the card body) links to a slug id, so following it changes
the URL for copying or sharing. Sections run largest-first: whole layout shells, then stateful domain
Views, then the object master/detail scaffolding, then conditional overlays
(modals/popovers/sheets shown open over a backdrop on the same grid), then every
primitive in its variant/state matrix, then the mobile counterparts
(`MobileGallery.svelte`, under a mobile `UiContext`). It is the visual drift/QA
surface (toggle the navbar theme for light/dark) and the source for the manual's
screenshots. The showcase stages consume the same samples, so demo data has one
home.

What the gallery must cover is a typed registry,
[`components/gallery/registry.ts`](../../../loca-website/src/components/gallery/registry.ts)
(`GALLERY_VIEWS` + `GALLERY_PRIMITIVES`). The renderer is hand-authored (one card
block per component, since each needs bespoke snippets and a natural background),
so it does NOT blindly iterate the registry: listing a name is necessary but not
sufficient. The walkers parse the registry AND assert the renderer actually
references each entry (a `SAMPLES.<Name>` / `P.<Name>` card), so both a
name-rendered-but-not-listed and a name-listed-but-not-rendered fail the build.
A View that is always rendered inside one parent's card (a pure structural
sub-piece the parent already shows: `ReasoningTraceView` inside `AgentMessageView`,
`SettingsContentView` inside `SettingsShellView`, ...) earns coverage
transitively via the `EMBEDDED_VIEWS` map instead of a redundant card of its own;
the walker exempts it from the card requirement and instead asserts the named
parent is galleried, rendered, and actually renders it.

## Icon + text rows (`IconText`)

Every icon-beside-short-text row (status lines, field errors, error/prompt
headers) renders through the shared `IconText` primitive
([primitives/IconText.svelte](components/primitives/IconText.svelte)), never a
hand-rolled `<i>` + `<span>` flex. It fixes the gap, the icon size (one step
above the text), the wrap-with-pinned-icon behavior, and the rule that a single
`color` paints both the icon and the text the same. When an inset card sits
below an `IconText` (an error's mono detail card, an errored field), give the
card's accent outline the SAME shade as that `IconText`'s text (errors:
`text-accent-red-700` + `outline-accent-red-700` / the `loca-error-ring`
helper).

**Always pass the FILLED icon variant to `IconText`** (e.g.
`i-material-symbols-error-rounded`, not `i-material-symbols-error-outline-rounded`).
A filled glyph reads as a deliberate badge at this small size. This is the one
deliberate exception to the app's usual outline-icon default (see the Icons
convention in [AGENTS.md](../../../../AGENTS.md)).

**No trailing period** on `IconText` text, even when it reads as a sentence
("[tool] wants to read a file", not "...a file."). It is a compact label.

## Interaction feedback (hover / press)

Clickables use the `hov:` (+1 shade) and `act:` (+2 shade) UnoCSS variants from
[uno-preset.ts](uno-preset.ts) with the `transition-interactive` shortcut, plus
the shared `use:ripple` action ([actions/ripple.ts](actions/ripple.ts)) for the
press splash. Two rules keep this working on touch:

- **`hov:` only brightens on a hover-capable pointer.** The variant's real
  `:hover` rule is gated behind `@media (hover: hover)`; only `[data-hover]`
  (the website demo cursor) is unguarded. So a "dim until hover" resting tone
  (e.g. `text-default-400 hov:text-default-700`) never brightens on a touch
  device and must not be the only legible state there.
- **Rest at the brighter shade on a coarse pointer.** A primitive whose resting
  tone relies on hover to become legible reads `useUiContext().pointer` and,
  when `"coarse"`, rests at the would-be hover shade (see `IconButton` `subtle`,
  `FlushSelect`, `SidebarItem`). Press feedback then comes from the ripple, not
  a color shift. Follow this for any new dim-until-hover control.

## Enforcement

Four walkers under `scripts/lint-plugins/`, run by `deno task lint` (strict, so a
violation fails the build). Together they make "every client component is visible
in the gallery" a hard rule with no exceptions:

- `check-shared-ui-purity` - no client/tauri/api imports in A0/A components.
- `check-view-coverage` - every `*View` is in `GALLERY_VIEWS`, has a NON-EMPTY
  bundle in `SAMPLES` (a `{}` bundle that would render zero cards fails), is
  actually rendered by the hand-authored renderer (a `SAMPLES.<Name>` card in
  Gallery.svelte/MobileGallery.svelte), and is wrapped by a client component or
  composed by another shared View (a website-only View is the single-source drift
  AGENTS.md forbids). A View listed in `EMBEDDED_VIEWS` instead is exempt from the
  card requirement and checked transitively: its named parent must be galleried,
  rendered, and must actually render it. Also asserts every `askuser/*` question
  sub-view is imported by `ToolCallView`, so it is shown transitively.
- `check-primitive-coverage` - every `primitives/*.svelte` is in
  `GALLERY_PRIMITIVES`, has a bundle in `PRIMITIVE_SAMPLES`, and is actually
  rendered (a `P.<Name>` card in Primitives.svelte).
- `check-component-tiers` - every client component is classified once in
  `.tiers.json`; a Tier B names an existing, galleried View; a Tier C carries no
  un-extracted raw styled markup (the **raw-leaf cap**: more than a few styled
  native leaves means markup that belongs in a shared View). The cap counts native
  elements with a visual utility class, plus raw native form controls
  (`select`/`input`/`textarea`, which belong in a shared primitive) and bespoke
  inline visual styles (`style:`/`style=` carrying mask/background/shadow/filter/
  clip-path, which the class scan is blind to). Any `composes` a Tier C declares
  must resolve to a galleried, imported View. The cap is the gate that stops
  bespoke client markup from escaping the gallery; extracting it into a `*View` is
  the fix.

## Layout

- `components/` - the A0 primitives and A `*View`s, in domain subfolders that
  mirror the client tree: `primitives/` (A0 leaves), `objects/`, `chat/`,
  `chat/messages/`, `chat/userinput/`, `settings/`. Import by deep file path
  (`@loca/shared/ui/components/chat/messages/FooView.svelte`); there are no
  barrels.
- `context.ts` - the `UiContext` interface, the `makeUiContext` factory, and
  `DEFAULT_UI_CONTEXT`.
- `samples/` - sample prop bundles, the `SAMPLES` registry, and the
  `OmitSnippetProps` helper (`types.ts`).
- `color.ts` / `animations.ts` / `types.ts` - presentational helpers and tokens.
- `uno-preset.ts` - the UnoCSS preset both apps spread.
- `styles/base.css` - design tokens (color ladders, bubble visuals, fonts).
