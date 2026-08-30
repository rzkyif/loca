// Core-side validation for the settings PATCH endpoint: type/range/regex checks
// per field, destination gating, and secret-key rejection. Also the shared
// regex-rule evaluator used by both PATCH validation and the client renderer.

import type { RegexValidation, SettingDestination, SettingField } from "./types.ts";
import { findField, isValidSettingKey, SECRET_KEYS } from "./schema.ts";
import { settingKeyDestination } from "./routing.ts";
import { modelFilesError } from "./model-files.ts";

/** True when `value` is type-compatible with `field`'s declared `type`.
 *  Render-only fields hold no persisted scalar, so they pass. */
function settingValueTypeOk(field: SettingField, value: unknown): boolean {
  switch (field.type) {
    case "boolean":
      return typeof value === "boolean";
    case "number":
    case "float":
    case "number_slider":
      return typeof value === "number" && Number.isFinite(value);
    case "select":
      return typeof value === "string" || typeof value === "number";
    case "command_preview":
      return typeof value === "string" || typeof value === "boolean";
    case "string":
    case "password":
    case "multiline":
    case "color":
    case "shortcut":
    case "preset":
    case "model_preset":
    case "stt_preset":
    case "tts_preset":
      return typeof value === "string";
    case "services":
    case "storage":
    case "object_management":
      return true;
  }
}

/**
 * Validate a PATCH body for `PATCH /api/v1/settings`, returning human-readable
 * errors (empty means acceptable). Every key must be a known schema key whose
 * destination is in `allow`, so the per-client overlay path passes
 * `["core", "client-on-core"]` and still rejects local-only keys. Secret-typed
 * and render-only keys are rejected outright; `null`/`undefined` are deletions
 * (reset to default) and pass on any accepted key.
 */
export function validateSettingsPatch(
  patch: Record<string, unknown>,
  opts: { allow: SettingDestination[] } = { allow: ["core"] },
): string[] {
  const errors: string[] = [];
  const secretSet = new Set<string>(SECRET_KEYS);
  const allowed = new Set<SettingDestination>(opts.allow);
  for (const [key, value] of Object.entries(patch)) {
    if (!isValidSettingKey(key)) {
      errors.push(`"${key}" is not a known setting`);
      continue;
    }
    if (secretSet.has(key)) {
      errors.push(`"${key}" is a secret and must be set via the secrets endpoint, not settings`);
      continue;
    }
    const dest = settingKeyDestination(key);
    if (!dest || !allowed.has(dest)) {
      errors.push(`"${key}" is not accepted on this settings path`);
      continue;
    }
    const field = findField(key);
    if (!field) continue;
    if (
      field.type === "command_preview" ||
      field.type === "services" ||
      field.type === "storage" ||
      field.type === "object_management"
    ) {
      errors.push(`"${key}" is a render-only field and holds no persisted value`);
      continue;
    }
    if (value === null || value === undefined) continue;
    if (!settingValueTypeOk(field, value)) {
      errors.push(`"${key}" has the wrong type for a "${field.type}" setting`);
      continue;
    }
    // A select value must be one of the declared (static) options; a select
    // backed by a runtime optionsSource can't be checked here and is skipped.
    if (field.type === "select" && "options" in field && Array.isArray(field.options)) {
      const allowed = field.options.map((o) => o.value);
      if (!allowed.includes(value as string | number)) {
        errors.push(`"${key}" must be one of: ${allowed.map((v) => String(v)).join(", ")}`);
        continue;
      }
    }
    // A slider value must fall inside its declared range.
    if (field.type === "number_slider" && typeof value === "number") {
      if (value < field.min || value > field.max) {
        errors.push(`"${key}" must be between ${field.min} and ${field.max}`);
        continue;
      }
    }
    // A plain number/float with declared bounds must respect them too, so an
    // out-of-range value can't reach the core and flow into a sidecar argv.
    if ((field.type === "number" || field.type === "float") && typeof value === "number") {
      if (field.min !== undefined && value < field.min) {
        errors.push(`"${key}" must be at least ${field.min}`);
        continue;
      }
      if (field.max !== undefined && value > field.max) {
        errors.push(`"${key}" must be at most ${field.max}`);
        continue;
      }
    }
    if (
      (field.type === "string" ||
        field.type === "password" ||
        field.type === "multiline" ||
        field.type === "number" ||
        field.type === "float") &&
      field.regex
    ) {
      const re = getValidationError(field.regex, value);
      if (re) errors.push(`"${key}": ${re}`);
    }
    if (key === "stt.modelFiles" || key === "tts.modelFiles") {
      const e = modelFilesError(value);
      if (e) errors.push(`"${key}": ${e}`);
    }
  }
  return errors;
}

export function getValidationError(regex: RegexValidation, value: unknown): string | null {
  if (value === undefined || value === null || value === "") return null;

  const strValue = String(value);

  if (typeof regex === "string") {
    try {
      if (!new RegExp(regex).test(strValue)) {
        return "Invalid format";
      }
    } catch {
      return "Invalid regex pattern";
    }
  } else if (Array.isArray(regex)) {
    for (const rule of regex) {
      try {
        if (!new RegExp(rule.regex).test(strValue)) {
          return rule.errorMessage;
        }
      } catch {
        return "Invalid regex pattern";
      }
    }
  }
  return null;
}
