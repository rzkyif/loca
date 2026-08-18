// Emits the JSON Schema bundle for the Zod half of the wire contract: every
// shape the core validates at a trust boundary.
//
// Zod -> z.toJSONSchema() -> the shared normalisation passes -> typify. The
// passes and the reason each one exists are in `normalise.ts`; every one of
// them fixes a mismatch that degrades the output silently.
//
// Why this half is worth generating from Zod rather than from the TypeScript:
// typify carries Zod's .min / .max / .int / .regex constraints into a
// `Deserialize` that enforces them, so deserializing the generated type IS the
// validation. TypeScript cannot express any of them. See D-003.
//
// Run via `deno task contract`, not directly.

import { z } from "zod";

import { audit, type Node, normalise, summary } from "./normalise.ts";

import * as chat from "../../packages/loca-shared/src/validation/chat.ts";
import * as pairing from "../../packages/loca-shared/src/validation/pairing.ts";
import * as session from "../../packages/loca-shared/src/validation/session.ts";
import * as locaJson from "../../packages/loca-shared/src/validation/loca-json.ts";
import * as sched from "../../packages/loca-shared/src/validation/scheduled-prompt.ts";
import * as extensions from "../../packages/loca-shared/src/validation/extensions.ts";
import * as models from "../../packages/loca-shared/src/validation/models.ts";
import * as memories from "../../packages/loca-shared/src/validation/memories.ts";
import * as mcp from "../../packages/loca-shared/src/validation/mcp.ts";
import * as greetings from "../../packages/loca-shared/src/validation/greetings.ts";
import * as catalog from "../../packages/loca-shared/src/domain/catalog.ts";

// `validation/ws.ts` is deliberately absent. Its union validates the
// server->client direction, which the core PRODUCES; the strongly-typed
// producer shape comes from the TypeScript half instead. That union is the
// client's parser and its payloads are intentionally loose (z.unknown()), so
// generating Rust from it would hand the core a frame type that cannot carry a
// Message or a Session. See D-006.
// `domain/catalog.ts` is here rather than in the TypeScript half despite living
// under `domain/`, because it is Zod first: every type in it is a
// `z.infer<typeof ...>` alias. Two consequences, both load-bearing. The catalog
// is DOWNLOADED and parsed, so it is a trust boundary and belongs on the half
// that generates an enforcing `Deserialize`. And a module of `z.infer` aliases
// makes ts-json-schema-generator fall over resolving zod's internals, so the
// TypeScript half cannot read it even if we wanted it to.
const modules: Record<string, Record<string, unknown>> = {
  chat,
  pairing,
  session,
  locaJson,
  sched,
  extensions,
  models,
  memories,
  mcp,
  greetings,
  catalog,
};

// Keyed by the name the Rust type will carry. The export name alone is unique
// across the five modules today; a future collision is a hard error rather than
// a silently dropped type, and is fixed by renaming the Zod export.
const defs: Record<string, z.ZodType> = {};
const origin: Record<string, string> = {};
for (const [mod, ns] of Object.entries(modules)) {
  for (const [name, value] of Object.entries(ns)) {
    if (!(value instanceof z.ZodType)) continue;
    const rust = pascal(name);
    if (rust in defs) {
      console.error(
        `name collision: ${mod}.${name} and ${origin[rust]} both become ${rust}. ` +
          "Rename one of the Zod exports.",
      );
      Deno.exit(1);
    }
    defs[rust] = value;
    origin[rust] = `${mod}.${name}`;
  }
}
if (Object.keys(defs).length === 0) {
  console.error(
    "no Zod schemas discovered. A second copy of zod in the module graph breaks the " +
      "instanceof check; make sure the root import map pins the same version as @loca/shared.",
  );
  Deno.exit(1);
}

// A schema embedded in N parents is inlined N times by default, so one Zod
// shape becomes N structurally identical but mutually incompatible Rust types
// (scheduleSpecSchema appears in 4 parents). Registering the *instance*
// globally gives it a $defs id, so every use site becomes a $ref to one type.
// Register the union itself, never a union's variants: a variant behind a $ref
// is what makes typify fall back to `untagged`.
const shared: Record<string, z.ZodType> = {
  ScheduleSpec: sched.scheduleSpecSchema,
  ScheduledPromptDraft: sched.scheduledPromptDraftSchema,
  // Also fixes a name collision: `toolPermissionsSchema` is exported AND nested
  // inside `toolSchema`, and typify auto-names the nested copy from its path,
  // landing on the same `LocaJsonToolPermissions` as the exported one and
  // emitting the type twice. Registering it collapses both to one $ref.
  LocaJsonToolPermissions: locaJson.toolPermissionsSchema,
};
for (const [id, schema] of Object.entries(shared)) z.globalRegistry.add(schema, { id });

// The same inlining, one level up: a schema that is EXPORTED under its own name
// and also nested inside another exported schema is still inlined at the nested
// site, so the named type and the nested copy become unrelated Rust types.
//
// Left alone this is not a cosmetic problem, it makes the generated module
// unusable for exactly the packages that need it most. The model catalog is
// reachable by two paths (`catalogPayloadSchema` and `modelCatalogSchema`), so
// every shape underneath it was emitted three times over: 28 structs under
// `CatalogPayload*`, 28 more under `ModelCatalog*`, and a third partial family
// under the plain names, with nothing converting between them. A function
// taking a quant ends up spelled
// `CatalogPayloadFamiliesItemModelsItemVariantsItemQuantsItem`, and a
// verify-then-use over the two roots cannot share a type at all. That is what
// kept `models/wire.rs` hand-written after the types nominally existed.
//
// So every exported schema is registered, not just the three above. The one
// rule that still has to hold is the union caveat: a branch registered behind a
// `$ref` is what makes typify give up and emit `untagged`, so any schema that
// appears as a union option is skipped. It is collected rather than
// hand-listed, because "which schemas are union branches" is a fact about the
// schemas and drifts the moment someone adds one.
const unionBranches = new Set<z.ZodType>();
function collectUnionBranches(schema: unknown, seen = new Set<unknown>()): void {
  if (!schema || typeof schema !== "object" || seen.has(schema)) return;
  seen.add(schema);
  const def = (schema as { _zod?: { def?: Record<string, unknown> } })._zod?.def;
  if (def?.type === "union" && Array.isArray(def.options)) {
    for (const option of def.options) unionBranches.add(option as z.ZodType);
  }
  for (const value of Object.values(def ?? {})) {
    if (Array.isArray(value)) value.forEach((v) => collectUnionBranches(v, seen));
    else collectUnionBranches(value, seen);
  }
}
for (const schema of Object.values(defs)) collectUnionBranches(schema);

let registered = 0;
for (const [name, schema] of Object.entries(defs)) {
  if (z.globalRegistry.has(schema) || unionBranches.has(schema)) continue;
  z.globalRegistry.add(schema, { id: name });
  registered++;
}

// One conversion for the whole set, so the registered shared schemas land in a
// single $defs that every use site can $ref. Each schema keeps the default
// `reused: "inline"`: "ref" hoists bare primitives (a twice-used z.string()
// becomes __schema0), which typify renders as useless newtype aliases.
const root = z.toJSONSchema(z.object(defs as never), {
  target: "draft-2020-12",
  unrepresentable: "any",
  io: "input",
}) as Node;

// typify emits one Rust type per $defs entry, so each top-level schema is
// lifted out of `properties` into $defs under the name we want its Rust type to
// carry.
const $defs = (root.$defs ?? {}) as Node;
for (const [name, schema] of Object.entries(root.properties as Node)) {
  // A registered schema is already in $defs under its id and appears here only
  // as a $ref to it. Lifting that ref would write
  // `$defs.X = { $ref: "#/$defs/X" }` whenever the derived name equals the
  // registered id: a self-cycle that sends typify into a stack overflow.
  if ((schema as Node)?.$ref) continue;
  $defs[name] = schema;
}
const bundle: Node = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $defs,
};

function pascal(s: string): string {
  return s.replace(/Schema$/, "").replace(/(^|[_-])([a-z])/g, (_, __, c) => c.toUpperCase());
}

const stats = normalise(bundle, $defs, "#/$defs/");
const warnings = audit(bundle, $defs);

const out = Deno.args[0];
if (!out) {
  console.error("usage: emit-zod.ts <out.json>");
  Deno.exit(2);
}
await Deno.writeTextFile(out, JSON.stringify(bundle, null, 2));
console.log(
  `zod: ${Object.keys(defs).length} schemas -> ${Object.keys($defs).length} defs ` +
    `(${summary(stats)}${warnings ? `, ${warnings} warnings` : ""})`,
);
