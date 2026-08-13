/**
 * Per-field persistence routing: which destination (client-on-client /
 * client-on-core / core) a setting key is stored to. The single routing truth
 * for both the client save path and core's PATCH validation.
 */

import type { SettingDestination } from "./types.ts";
import { groupDestinations } from "./types.ts";
import { SETTINGS_SCHEMA } from "./schema.ts";

// Per-field persistence destination, honoring hybrid groups: a section's
// `destination` overrides its group's first listed destination (see
// groupDestinations). Built lazily once; the schema is immutable after import.
let _keyDestinations: Map<string, SettingDestination> | null = null;

function keyDestinations(): Map<string, SettingDestination> {
  if (_keyDestinations) return _keyDestinations;
  const map = new Map<string, SettingDestination>();
  for (const group of SETTINGS_SCHEMA) {
    const groupDest = groupDestinations(group)[0];
    for (const section of group.sections) {
      const dest = section.destination ?? groupDest;
      for (const field of section.fields) map.set(field.id, dest);
    }
  }
  _keyDestinations = map;
  return map;
}

/** Persistence destination (client-on-client / client-on-core / core) for a
 *  schema field id, honoring per-section overrides in hybrid groups. Undefined
 *  for unknown keys. The single routing truth for both the client save path and
 *  core's PATCH validation. */
export function settingKeyDestination(key: string): SettingDestination | undefined {
  return keyDestinations().get(key);
}
