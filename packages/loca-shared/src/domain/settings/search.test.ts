// Group visibility: the two gates and why they must stay separate.
//
// `desktopOnly` is about the DEVICE, `requiresCapability` is about the CORE.
// Collapsing them is the bug this whole mechanism exists to prevent, so these
// tests assert each one leaves the other alone. Introspects the live schema
// where it can, so the assertions survive a group being renamed.

import { assertEquals } from "@std/assert";
import { isGroupVisible } from "./search.ts";
import { SETTINGS_SCHEMA } from "./schema.ts";
import type { SettingGroup } from "./types.ts";
import type { CoreCapabilities } from "../../api/http.ts";

const EXTENSIONS = SETTINGS_SCHEMA.find((g) => g.id === "extensions") as SettingGroup;
const SHORTCUTS = SETTINGS_SCHEMA.find((g) => g.id === "shortcuts") as SettingGroup;

/** A capability probe answering `false` for exactly one flag. */
function allBut(off: keyof CoreCapabilities) {
  return (flag: keyof CoreCapabilities) => flag !== off;
}

Deno.test("a group is dropped when the connected core lacks its capability", () => {
  assertEquals(isGroupVisible(EXTENSIONS, "desktop", allBut("extensions")), false);
  assertEquals(isGroupVisible(EXTENSIONS, "mobile", allBut("extensions")), false);
});

Deno.test("the same group is kept when the core has the capability", () => {
  assertEquals(
    isGroupVisible(EXTENSIONS, "desktop", () => true),
    true,
  );
  // THE case the whole mechanism exists for: a phone paired to a desktop core
  // must still manage that core's extensions. If this ever reads false, the
  // capability has been confused with the platform again.
  assertEquals(
    isGroupVisible(EXTENSIONS, "mobile", () => true),
    true,
  );
});

Deno.test("an unrelated capability being false does not hide the group", () => {
  assertEquals(isGroupVisible(EXTENSIONS, "mobile", allBut("pairing")), true);
  assertEquals(isGroupVisible(EXTENSIONS, "mobile", allBut("localSpeech")), true);
});

Deno.test("no capability information at all leaves every group visible", () => {
  // The default is permissive on purpose: a core that predates negotiation
  // sends no capabilities block, and a pessimistic default would blank half its
  // settings panel. Called with two arguments, as every non-capability-aware
  // caller does.
  for (const group of SETTINGS_SCHEMA) {
    if (group.hidden || group.desktopOnly) continue;
    assertEquals(isGroupVisible(group, "desktop"), true, group.id);
  }
});

Deno.test("the device gate still works and is not confused with the core gate", () => {
  // Global shortcuts have no touch-shell equivalent. That is a fact about the
  // device, so a fully capable core does not bring them back on mobile.
  assertEquals(
    isGroupVisible(SHORTCUTS, "desktop", () => true),
    true,
  );
  assertEquals(
    isGroupVisible(SHORTCUTS, "mobile", () => true),
    false,
  );
});

Deno.test("hidden still wins over both gates", () => {
  // Built from a live group rather than found in the schema: no shipped group
  // sets `hidden` today, so searching for one finds nothing and the test would
  // pass against undefined. The ordering is what is being pinned - `hidden`
  // short-circuits before either gate is consulted.
  const hidden: SettingGroup = { ...EXTENSIONS, hidden: true };
  assertEquals(
    isGroupVisible(hidden, "desktop", () => true),
    false,
  );
  assertEquals(isGroupVisible(hidden, "mobile", allBut("extensions")), false);
});
