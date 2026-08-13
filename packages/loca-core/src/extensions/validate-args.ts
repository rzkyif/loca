// Host-side validation of LLM-emitted tool arguments against the tool's
// declared JSON-Schema `parameters`, with default-filling.
//
// The core/client split dropped this step: `argumentsJson` was forwarded to the
// worker verbatim, so a tool no longer received the "normalized args with
// defaults applied" its author declared (and malformed model output reached
// tool code uncaught). This restores it, mirroring the old Bun extension
// service's Ajv usage.

import { Ajv, type AnySchema, type ValidateFunction } from "ajv";
// No npm types ship for safe-regex; the default export is a predicate that
// flags catastrophic-backtracking regexes (nested quantifiers / high star
// height).
import safeRegexUntyped from "safe-regex";
import type { Tool } from "@loca/shared";
import { AppError } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";

const log = getLogger("extensions.validate");

const isSafeRegex = safeRegexUntyped as (re: string | RegExp, opts?: { limit?: number }) => boolean;

// A tool author controls the `parameters` schema, including JSON-Schema
// `pattern` / `patternProperties` regexes. Ajv compiles those into the
// validator and runs them SYNCHRONOUSLY against LLM-emitted argument strings on
// the core's single event loop, so a catastrophic-backtracking pattern (e.g.
// `(a+)+$`) plus a pathological argument would pin the loop and freeze every
// client. A synchronous regex can't be interrupted by a timer, so the only real
// defense is to keep the dangerous regex from ever compiling: we strip any
// unsafe `pattern`/`patternProperties` regex from the schema before compiling
// (everything else still validates + fills defaults).
function isUnsafePattern(p: unknown): boolean {
  if (typeof p !== "string") return false;
  try {
    return !isSafeRegex(p);
  } catch {
    return true; // unparseable regex -> treat as unsafe and strip it
  }
}

function stripUnsafePatterns(node: unknown, stripped: string[]): unknown {
  if (Array.isArray(node)) {
    return node.map((n) => stripUnsafePatterns(n, stripped));
  }
  if (node && typeof node === "object") {
    const out: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(node as Record<string, unknown>)) {
      // `pattern` can appear at any depth (properties.*, items, propertyNames,
      // $defs, ...); the recursion below reaches all of them.
      if (key === "pattern" && isUnsafePattern(value)) {
        stripped.push(String(value));
        continue;
      }
      if (key === "patternProperties" && value && typeof value === "object") {
        const safe: Record<string, unknown> = {};
        for (const [re, sub] of Object.entries(value as Record<string, unknown>)) {
          if (isUnsafePattern(re)) {
            stripped.push(re);
            continue;
          }
          safe[re] = stripUnsafePatterns(sub, stripped);
        }
        out[key] = safe;
        continue;
      }
      out[key] = stripUnsafePatterns(value, stripped);
    }
    return out;
  }
  return node;
}

// `useDefaults` fills missing properties' defaults during validate(); it
// mutates the data object in place, so we re-serialize afterward. `strict:
// false` tolerates the assorted JSON-Schema dialects tool authors emit.
const ajv = new Ajv({ strict: false, allErrors: true, useDefaults: true });

// Compiled-validator cache keyed by tool id. A tool's parameters schema can
// change across re-installs, so we stash the schema JSON and recompile when it
// differs. Bounded to avoid unbounded growth; eviction is FIFO via Map
// insertion order (good enough; validators are cheap to rebuild).
const MAX_VALIDATORS = 256;
const cache = new Map<string, { schemaJson: string; validate: ValidateFunction }>();

/** Validate `argumentsJson` against `tool.parameters` and return a normalized
 *  JSON string with schema defaults applied. Throws
 *  `AppError("validation_error")` when the arguments are not valid JSON or
 *  violate the schema. A malformed author schema is treated as "no validation"
 *  (raw args pass through) so a bad schema can't wedge the call path. */
export function validateAndNormalizeToolArgs(tool: Tool, argumentsJson: string): string {
  let parsed: unknown;
  try {
    parsed = argumentsJson.trim() === "" ? {} : JSON.parse(argumentsJson);
  } catch {
    throw new AppError("validation_error", `tool ${tool.name}: arguments are not valid JSON`);
  }

  const schema = tool.parameters ?? { type: "object" };
  const schemaJson = JSON.stringify(schema);
  let entry = cache.get(tool.id);
  if (!entry || entry.schemaJson !== schemaJson) {
    let validate: ValidateFunction;
    try {
      const stripped: string[] = [];
      const safeSchema = stripUnsafePatterns(schema, stripped);
      if (stripped.length > 0) {
        log.warn(
          `tool ${tool.id}: stripped ${stripped.length} unsafe regex pattern(s) from parameters schema (ReDoS guard)`,
        );
      }
      validate = ajv.compile(safeSchema as AnySchema);
    } catch (err) {
      log.warn(
        `tool ${tool.id}: unusable parameters schema, skipping validation: ${
          err instanceof Error ? err.message : err
        }`,
      );
      return argumentsJson;
    }
    if (cache.size >= MAX_VALIDATORS) {
      const oldest = cache.keys().next().value;
      if (oldest !== undefined) cache.delete(oldest);
    }
    entry = { schemaJson, validate };
    cache.set(tool.id, entry);
  }

  if (!entry.validate(parsed)) {
    const detail = ajv
      .errorsText(entry.validate.errors, { separator: "; " })
      .replace(/^data/, "args");
    throw new AppError("validation_error", `tool ${tool.name}: invalid arguments: ${detail}`);
  }
  return JSON.stringify(parsed);
}

export function __resetValidatorCacheForTesting(): void {
  cache.clear();
}
