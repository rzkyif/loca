# Settings

The settings schema lives here and is the single source of truth for both the
client renderer and the core. The layout:

- [`groups/`](groups/): one module per settings group (General, Appearance, LLM,
  ...). These are the data; each file exports its group definition.
- [`engine.ts`](engine.ts): composition root. Re-exports the runtime split
  across focused siblings so both sides have one entry point:
  [`schema.ts`](schema.ts) (composes the groups into the one schema, derived
  constants, defaults, field lookup), [`routing.ts`](routing.ts) (per-field
  destination), [`conditions.ts`](conditions.ts) (`visibleWhen`/`editableWhen`
  evaluation + dependency map), [`search.ts`](search.ts) (visibility helpers +
  the search index), [`validation.ts`](validation.ts) (core-side PATCH
  validation), and [`model-files.ts`](model-files.ts) (settings that reference
  downloadable weights).
- [`types.ts`](types.ts): the field/section/group primitives. The structural
  mechanics (field/section/group shapes, `descriptionTier`, `destination`) are
  documented on the types themselves.
- [`index.ts`](index.ts): the barrel re-exporting the schema and types.

Every `name` and `description` in a group ships straight to users, so treat them
as product copy: the rules for writing them live in the root
[`COPY.md`](../../../../../COPY.md), which governs user-facing copy across the
whole product. The rest of this file is the architecture.

## Architecture

How a setting value moves through the system, end to end.

### Destinations and routing

Every field persists to exactly one destination, resolved by
`settingKeyDestination()` in [`routing.ts`](routing.ts): a section's `destination`
override wins (hybrid groups), otherwise the group's first listed destination.
That helper is the single routing truth for the client's save path and the
core's PATCH validation alike. There are three storage locations but only two
user-visible labels (the header chip collapses any `client-on-*` to "Client",
`core` to "Core", via `destinationLabel()`):

- **client-on-client** keys live in `~/.loca/<channel>/client/settings.json`.
  Pure device preferences the core never reads (appearance, shortcuts, the
  display-only toggles, greetings).
- **client-on-core** keys live in the core's `client_settings` SQLite table,
  keyed by `clients.id`. Per-client preferences the core must apply server-side
  (the per-turn inference knobs: sampling, system/helper prompts, tool and
  memory selection), including for scheduled/automated sessions that have no
  client frame. The core reads them via `loadEffective(clientId)` = the shared
  `core` store overlaid with this client's overrides.
- **core** keys live in `~/.loca/<channel>/core/settings.json` on the paired
  core. Shared physical resources (the model/server/backend, sidecars, tool
  worker pool, installed extensions/MCP, the memory store).

`client-on-core` and `core` are both reached via `GET`/`PATCH
/api/v1/settings`: GET returns this client's effective view, and PATCH
partitions the body by destination, writing `core` keys to the shared store and
`client-on-core` keys to the client's overlay. `client-on-core` changes
broadcast `settings.updated` ONLY to the owning client (never to others); `core`
changes broadcast to all.

All stores are sparse: only non-default values are persisted, and a `null` in a
PATCH deletes the key (reverts it to the shared/default value). The core PATCH
is strict (`validateSettingsPatch(patch, { allow })`): unknown keys,
`client-on-client` keys, render-only fields, secret-typed keys, and wrong-typed
values are all rejected, and GET responses are sanitized to the core-stored
schema keys, so the core store can never carry junk to clients. Core-internal
state (e.g. the built-in extension seed marker) lives outside the settings store
entirely.

### The client store

`settingsState` (`packages/loca-client/src/ui/lib/state/settings.svelte.ts`)
keeps three layers: schema defaults, the client sparse layer, and the core
sparse layer. The merged record (`currentSettings`) is the one reactive surface
every reader consumes. Every mutation, whatever its origin, flows through one
pipeline that updates the owning layer, mutates the merged record per key, and
notifies `onChange` listeners only for keys whose effective value actually
changed:

- **user**: an edit from the settings UI or a programmatic `updateSetting`.
  Applied optimistically, debounced, then persisted per destination (file write,
  core PATCH diff against the last core-confirmed state, dirty secrets to the
  vault). A failed destination rolls back only its own keys, firing the reverse
  transitions.
- **load**: the client file at boot, and the core baseline GET whenever a core
  is selected or a connection (re)establishes. Core edits made before the
  baseline lands are queued and PATCHed right after it, never dropped.
- **remote**: a `settings.updated` WS frame. The core broadcasts every store
  change (another client's PATCH, a preset apply, a factory reset) as a sparse
  delta plus deleted keys; the value-diff makes the echo of this client's own
  PATCH a no-op, which is also what lets listener writebacks converge.

Side effects (arming TTS, stopping VAD, mutual-exclusion writebacks) live in
`settings-effects.ts` as `onChange` listeners, so they react identically to a
user toggle, a pairing's baseline, and a remote change.

### Secrets

Secret-typed (password) fields never enter the layers, the file, or a PATCH.
Values go straight to the core's encrypted vault via the secrets endpoints; the
core only ever returns the configured NAMES (`GET /settings/secrets`, mirrored
in the `secretNames` field of `settings.updated`), which drive the "saved"
placeholder in password fields. Only fields the user actually edited in the
session are written, so an unrelated save can't clear a vault entry.

### Client file tenancy

The client keeps one file per concern under `~/.loca/<channel>/client/`, one
owner per file, so no two modules ever read-modify-write the same data:

- `settings.json`: the sparse client settings (owned by `settingsState`).
- `cores.json`: the paired-cores registry + current-core pointer (owned by
  `lib/core/cores.ts`).
- `snippets/<name>.json`: one file per snippet; the directory listing is the
  registry, the filename stem is the snippet id, so sharing a snippet is copying
  its file in and rescanning from the Snippets manager menu.
