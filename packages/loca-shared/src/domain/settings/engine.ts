/**
 * Composition root for the settings runtime. The schema is assembled from the
 * per-group modules under `./groups/`; the mechanics are split across focused
 * siblings and re-exported here so both the client renderer and core's setting
 * reads have one entry point:
 *
 * - `./schema.ts`: the composed `SETTINGS_SCHEMA`, derived constants
 *   (`SECRET_KEYS`, `SETTING_IDS`), defaults, and field lookup.
 * - `./routing.ts`: per-field destination (client-on-client / client-on-core /
 *   core) resolution.
 * - `./conditions.ts`: `visibleWhen`/`editableWhen` evaluation and the reverse
 *   dependency map.
 * - `./search.ts`: visibility helpers and the settings search index.
 * - `./validation.ts`: core-side PATCH validation.
 * - `./model-files.ts`: helpers for settings that reference downloadable model
 *   weights.
 */

export * from "./schema.ts";
export * from "./routing.ts";
export * from "./conditions.ts";
export * from "./search.ts";
export * from "./validation.ts";
export * from "./model-files.ts";
