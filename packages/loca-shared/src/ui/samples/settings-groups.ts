import { getDefaultSettings, SETTINGS_SCHEMA } from "../../domain/settings/engine.ts";

// The real settings groups (sidebar order, names, outline icons) straight from
// the schema; hidden groups (e.g. cores) are omitted, exactly as in the app.
// Shared by every settings-shell/sidebar/content sample so they stay in lockstep.
export const SAMPLE_GROUPS = SETTINGS_SCHEMA.filter((g) => !g.hidden).map((g) => ({
  id: g.id,
  name: g.name,
  icon: g.icon,
  iconInactive: g.iconInactive ?? g.icon,
}));

/** Every setting id -> its schema default, the canonical "fresh app" values
 *  the static field renderers read. */
export const SAMPLE_VALUES = getDefaultSettings();

/** The first visible group's id (the default-selected sidebar entry). */
export const SAMPLE_FIRST_GROUP = SAMPLE_GROUPS[0]?.id ?? "";
