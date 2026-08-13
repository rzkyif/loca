// Tests for the core-side settings PATCH validator. Introspects the live
// schema so the assertions survive field renames.

import { assertEquals } from "@std/assert";
import {
  isSecretSettingKey,
  SECRET_KEYS,
  settingKeyDestination,
  SETTINGS_SCHEMA,
  validateSettingsPatch,
} from "./engine.ts";
import {
  CLIENT_GROUP_IDS,
  CORE_GROUP_IDS,
  destinationLabel,
  groupDestinations,
  isClientGroup,
  isCoreGroup,
  type SettingField,
} from "./types.ts";

// A group carries the "Client" label if any destination collapses to it
// (client-on-client OR client-on-core), and "Core" if any is the shared core.
const hasClientLabel = (g: { destination: unknown }) =>
  groupDestinations(g as Parameters<typeof groupDestinations>[0]).some(
    (d) => destinationLabel(d) === "Client",
  );
const hasCoreLabel = (g: { destination: unknown }) =>
  groupDestinations(g as Parameters<typeof groupDestinations>[0]).some(
    (d) => destinationLabel(d) === "Core",
  );

Deno.test("setting ids: a repeated id must map to the SAME field, never conflicting definitions", () => {
  // Field ids are persisted on disk and on the wire, and validation / defaults /
  // destination all key off the id. A duplicate id is allowed ONLY for the
  // deliberate shared-field pattern (one persisted value rendered in two sections,
  // e.g. `speech.binaryBackend` under both STT and TTS, since they share one
  // loca-core-speech process). Those occurrences must be identical apart from the
  // UI-only `visibleWhen`, or the value would resolve inconsistently depending on
  // which occurrence wins. A genuinely accidental collision (two different-typed or
  // different-default fields sharing an id) fails here.
  const byId = new Map<string, SettingField[]>();
  for (const group of SETTINGS_SCHEMA) {
    for (const section of group.sections) {
      for (const field of section.fields) {
        const list = byId.get(field.id) ?? [];
        list.push(field);
        byId.set(field.id, list);
      }
    }
  }
  // Normalize away the UI-only visibleWhen; everything else (type, defaultValue,
  // options, validation) must match across occurrences of one id.
  const norm = (f: SettingField) => JSON.stringify({ ...f, visibleWhen: undefined });
  const conflicts = [...byId.entries()]
    .filter(([, fields]) => fields.length > 1 && new Set(fields.map(norm)).size > 1)
    .map(([id]) => id);
  assertEquals(conflicts, [], `setting ids with conflicting definitions: ${conflicts.join(", ")}`);
});

Deno.test("group-id registry stays in sync with schema destinations (no drift)", () => {
  // CLIENT_GROUP_IDS / CORE_GROUP_IDS are hand-maintained for their literal
  // types; this is the derive-check that fails if a group's `destination` in the
  // schema ever disagrees with the registry. A multi-destination group is
  // listed in both registries.
  const schemaClient = SETTINGS_SCHEMA.filter(hasClientLabel)
    .map((g) => g.id)
    .sort();
  const schemaCore = SETTINGS_SCHEMA.filter(hasCoreLabel)
    .map((g) => g.id)
    .sort();
  assertEquals(schemaClient, [...CLIENT_GROUP_IDS].sort());
  assertEquals(schemaCore, [...CORE_GROUP_IDS].sort());
  // The classifier helpers must agree with each group's declared destination(s).
  for (const g of SETTINGS_SCHEMA) {
    assertEquals(isClientGroup(g.id), hasClientLabel(g));
    assertEquals(isCoreGroup(g.id), hasCoreLabel(g));
  }
});

Deno.test("hybrid groups give every section a destination and label every header", () => {
  // A group spanning more than one destination routes per section, so each
  // section MUST declare its own destination (so its fields persist to the right
  // place). It must also be labeled IF it renders a collapsible header - i.e. it
  // has at least one visible config field. A full-bleed object_management
  // manager and an all-hidden flag section render no header, so they need no
  // label.
  const NEVER = "__never__";
  const rendersHeader = (section: { fields: SettingField[] }) =>
    section.fields.some((f) => {
      if (f.type === "object_management") return false;
      const vw = (f as { visibleWhen?: { eq?: unknown } }).visibleWhen;
      if (vw && "eq" in vw && vw.eq === NEVER) return false;
      return true;
    });
  for (const group of SETTINGS_SCHEMA) {
    if (!Array.isArray(group.destination)) continue;
    for (const section of group.sections) {
      assertEquals(
        section.destination !== undefined,
        true,
        `a section in hybrid group "${group.id}" needs a destination`,
      );
      if (rendersHeader(section)) {
        assertEquals(
          typeof section.label === "string" && section.label.length > 0,
          true,
          `hybrid group "${group.id}" has an unlabeled header section`,
        );
      }
    }
  }
});

Deno.test("validateSettingsPatch: accepts a well-typed core-destination value", () => {
  // tts.enabled lives in the tts group's core section (hybrid group).
  assertEquals(validateSettingsPatch({ "tts.enabled": true }), []);
});

Deno.test("validateSettingsPatch: rejects a wrong-typed known value", () => {
  assertEquals(validateSettingsPatch({ "tts.enabled": "not-a-boolean" }).length > 0, true);
});

Deno.test("validateSettingsPatch: rejects secret-typed keys (vault only)", () => {
  const secret = SECRET_KEYS[0];
  assertEquals(typeof secret, "string");
  assertEquals(validateSettingsPatch({ [secret]: "sk-leak" }).length > 0, true);
});

Deno.test("validateSettingsPatch: rejects unknown keys, allows deletions on core keys", () => {
  // The core store holds only known core-destination keys; null is a reset.
  assertEquals(validateSettingsPatch({ "totally.unknown.key": "x" }).length > 0, true);
  assertEquals(validateSettingsPatch({ "totally.unknown.key": null }).length > 0, true);
  assertEquals(validateSettingsPatch({ "llm.modelPath": null }), []);
});

Deno.test("validateSettingsPatch: rejects client-destination keys", () => {
  assertEquals(validateSettingsPatch({ "appearance.theme": "dark" }).length > 0, true);
  // The client section of a hybrid group routes to the client store too.
  assertEquals(validateSettingsPatch({ "tts.voice": "bf_emma" }).length > 0, true);
});

Deno.test("settingKeyDestination: honors group defaults and section overrides", () => {
  assertEquals(settingKeyDestination("llm.modelPath"), "core");
  // Hybrid tts group: core section vs client section.
  assertEquals(settingKeyDestination("tts.enabled"), "core");
  assertEquals(settingKeyDestination("tts.voice"), "client-on-client");
  // Client section override inside the otherwise core tools group.
  assertEquals(settingKeyDestination("extensions.skipRiskyGrantWarning"), "client-on-client");
  assertEquals(settingKeyDestination("appearance.theme"), "client-on-client");
  // Per-client core overlay: inference knobs the core applies per turn.
  assertEquals(settingKeyDestination("prompts.defaultSystemPrompt"), "client-on-core");
  assertEquals(settingKeyDestination("llm.temperature"), "client-on-core");
  assertEquals(settingKeyDestination("tools.maxTools"), "client-on-core");
  // ...but the shared physical resources in the same hybrid groups stay core.
  assertEquals(settingKeyDestination("llm.supportImages"), "core");
  assertEquals(settingKeyDestination("extensions.maxWarmWorkers"), "core");
  assertEquals(settingKeyDestination("tools.list"), "core");
  assertEquals(settingKeyDestination("totally.unknown.key"), undefined);
});

Deno.test("validateSettingsPatch: per-client overlay keys gated by allow-list", () => {
  // A client-on-core key is rejected on the default (core-only) path...
  assertEquals(validateSettingsPatch({ "prompts.defaultSystemPrompt": "hi" }).length > 0, true);
  // ...and accepted when the per-client overlay path opts it in.
  assertEquals(
    validateSettingsPatch(
      { "prompts.defaultSystemPrompt": "hi" },
      { allow: ["core", "client-on-core"] },
    ),
    [],
  );
  // A local-only client key is rejected on both paths (it never reaches core).
  assertEquals(
    validateSettingsPatch({ "appearance.theme": "dark" }, { allow: ["core", "client-on-core"] })
      .length > 0,
    true,
  );
});

Deno.test("isSecretSettingKey: true for password fields, false otherwise", () => {
  assertEquals(isSecretSettingKey(SECRET_KEYS[0]), true);
  assertEquals(isSecretSettingKey("definitely.not.a.secret"), false);
});
