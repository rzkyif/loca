// Unit coverage for the Share import/export logic. Field ids are derived from
// the live schema rather than hard-coded so a settings rename can't rot the
// test.

import { describe, expect, it } from "vitest";
import { getDefaultSettings, isValidSettingKey, SECRET_KEYS } from "@loca/shared";
import {
  buildTree,
  classifyImport,
  computeExportJson,
  EXPORTABLE_FIELD_IDS,
  parseImport,
} from "./share";

const DEFAULTS = getDefaultSettings();
// A stable, real exportable field id to exercise the value-level helpers.
const SAMPLE_ID = [...EXPORTABLE_FIELD_IDS][0];

describe("EXPORTABLE_FIELD_IDS", () => {
  it("only contains valid setting keys", () => {
    for (const id of EXPORTABLE_FIELD_IDS) {
      expect(isValidSettingKey(id)).toBe(true);
    }
  });

  it("excludes every secret key", () => {
    for (const key of SECRET_KEYS) {
      expect(EXPORTABLE_FIELD_IDS.has(key)).toBe(false);
    }
  });
});

describe("buildTree", () => {
  it("keeps only allowed fields and drops empty groups", () => {
    const tree = buildTree(new Set([SAMPLE_ID]));
    const allFields = tree.flatMap((g) => [...g.fields, ...g.sections.flatMap((s) => s.fields)]);
    expect(allFields).toHaveLength(1);
    expect(allFields[0].id).toBe(SAMPLE_ID);
  });

  it("flags fields via warnFor", () => {
    const tree = buildTree(new Set([SAMPLE_ID]), (id) => id === SAMPLE_ID);
    const field = tree.flatMap((g) => [...g.fields, ...g.sections.flatMap((s) => s.fields)])[0];
    expect(field.warn).toBe(true);
    expect(field.disabled).toBe(false);
  });

  it("flags fields via disabledFor", () => {
    const tree = buildTree(new Set([SAMPLE_ID]), undefined, (id) => id === SAMPLE_ID);
    const field = tree.flatMap((g) => [...g.fields, ...g.sections.flatMap((s) => s.fields)])[0];
    expect(field.disabled).toBe(true);
    expect(field.warn).toBe(false);
  });

  it("returns nothing when no ids are allowed", () => {
    expect(buildTree(new Set())).toEqual([]);
  });
});

describe("computeExportJson", () => {
  it("includes only selected, non-default values", () => {
    const current = { ...DEFAULTS, [SAMPLE_ID]: "__changed__" };
    const json = computeExportJson(new Set([SAMPLE_ID]), current, DEFAULTS);
    expect(JSON.parse(json)).toEqual({ [SAMPLE_ID]: "__changed__" });
  });

  it("omits values left at their default", () => {
    const json = computeExportJson(new Set([SAMPLE_ID]), { ...DEFAULTS }, DEFAULTS);
    expect(JSON.parse(json)).toEqual({});
  });

  it("omits non-default values that are not selected", () => {
    const current = { ...DEFAULTS, [SAMPLE_ID]: "__changed__" };
    const json = computeExportJson(new Set(), current, DEFAULTS);
    expect(JSON.parse(json)).toEqual({});
  });
});

describe("parseImport", () => {
  it("returns empty for blank input with no error", () => {
    expect(parseImport("   ")).toEqual({ values: {}, unknownKeys: [] });
  });

  it("errors on non-JSON and on non-objects", () => {
    expect(parseImport("not json").error).toBeTruthy();
    expect(parseImport("[1,2,3]").error).toBeTruthy();
    expect(parseImport("42").error).toBeTruthy();
  });

  it("partitions importable keys from unknown ones", () => {
    const result = parseImport(JSON.stringify({ [SAMPLE_ID]: "v", "not.a.real.key": 1 }));
    expect(result.values).toEqual({ [SAMPLE_ID]: "v" });
    expect(result.unknownKeys).toEqual(["not.a.real.key"]);
    expect(result.error).toBeUndefined();
  });

  it("treats secret keys as unknown (excluded)", () => {
    if (SECRET_KEYS.length === 0) return;
    const secret = SECRET_KEYS[0];
    const result = parseImport(JSON.stringify({ [secret]: "sk-123" }));
    expect(result.values).toEqual({});
    expect(result.unknownKeys).toEqual([secret]);
  });
});

describe("classifyImport", () => {
  it("is a noop when the value is unchanged", () => {
    expect(classifyImport("a", "a", "default")).toBe("noop");
  });

  it("applies over a default value", () => {
    expect(classifyImport("new", "default", "default")).toBe("apply");
  });

  it("overwrites a customized value", () => {
    expect(classifyImport("new", "customized", "default")).toBe("overwrite");
  });
});
