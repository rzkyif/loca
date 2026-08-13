# @loca/shared

TypeScript types and Zod validators consumed by both core and client: the API
contract, domain shapes, the `loca.json` schema, and the WS frame unions. The
non-UI exports have no runtime side effects; importing them never touches the
network, filesystem, or globals. The wire-format rule: when changing the wire
format, update this package first, then adapt core and client to it.

The package also owns the shared UI layer under `./ui/*` (see Layout): the
design tokens, the UnoCSS preset, and the presentational Svelte components that
the client and the website both render so they look identical. These exports
depend on `svelte` and `unocss`; the core never imports `./ui/*`, so it stays
free of UI dependencies. The shared layer follows a defined system
(presentational backbone + a narrow `./ui/context` provider for ambient state
only, a four-tier component taxonomy, sample bundles, a website gallery, and
lint enforcement): [`src/ui/README.md`](src/ui/README.md) is the canonical
reference. The short version: per-instance data and callbacks arrive as
props/snippets; only ambient cross-cutting state (alignment, animation, theme,
platform) is read through the provider, never a client store directly.

## Layout

- `src/api/`: the HTTP route request/response types (`http.ts`), the WS frame
  discriminated unions (`ws.ts`), and the shared error codes (`errors.ts`).
- `src/domain/`: domain shapes: catalog, session, extension, model, prompts,
  storage, recommendations, and the settings system under `settings/` (engine,
  types, and one file per group in `groups/`).
- `src/validation/`: Zod schemas for inbound payloads, including `loca-json.ts`
  (the extension manifest), chat, pairing, session, and WS frames.
- `src/crypto/`: `pake.ts` (CPace PAKE used for pairing) and `canonical.ts`
  (canonical JSON serialization for manifest signing).
- `src/loca-json-schema.json`: the published JSON Schema for extension authors,
  generated from the Zod schema and served from `get.loca.rzkyif.com`.
- `src/ui/`: the shared UI layer. `styles/base.css` (color ladders, bubble
  visuals, roundedness, range/scrollbar chrome, fonts), `uno-preset.ts` (the
  shortcuts/rules/presets both apps' UnoCSS configs spread), `context.ts` (the
  state surface components read), `types.ts` / `animations.ts` (presentational
  types and timing constants), `components/` (the presentational Svelte
  components, e.g. `Bubble`, `Tabs`, `Button`, `IconButton`, and the `*View`
  compositions), and `samples/` (named sample prop bundles for every `*View`,
  consumed by the website gallery and showcase). See
  [`src/ui/README.md`](src/ui/README.md).

## Run, build, test

No build step; both core and client import the TypeScript sources directly. From
the repo root:

- `deno task test:shared`: run this package's tests.

## Further reading

- [`src/ui/README.md`](src/ui/README.md): the shared-UI system (pattern, tiers,
  the `*View` contract, samples, gallery, and lint enforcement).
- [`src/domain/settings/README.md`](src/domain/settings/README.md): the settings
  system architecture (schema, routing, persistence). Copy rules for settings
  strings live in the root [`COPY.md`](../../COPY.md).
- [`../loca-extension-builtin/README.md`](../loca-extension-builtin/README.md): extension author
  docs for the `loca.json` format.
