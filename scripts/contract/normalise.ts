// The normalisation passes both contract emitters run before handing a JSON
// Schema bundle to typify.
//
// typify reads a narrower dialect than either generator writes, and every
// mismatch degrades the output SILENTLY: the Rust still compiles, the types
// still deserialize something, and the defect surfaces as a mystery
// "data did not match any variant" in production. Each pass below exists
// because a specific mismatch was observed, and the round-trip fixtures in
// `packages/loca-core-rs/tests/` are what keep them honest.
//
// The two failure modes, both silent:
//
//   1. A union typify cannot see a discriminant in becomes
//      `#[serde(untagged)]`, which trial-parses every variant and names them
//      Variant0, Variant1, ...
//   2. An `anyOf` typify cannot resolve at all becomes a struct of
//      `#[serde(flatten)] Option<SubtypeN>` fields, which accepts almost
//      nothing and reports "invalid type: string, expected struct X".

export type Node = Record<string, unknown>;

export interface Stats {
  rounds: number;
  flattened: number;
  constToEnum: number;
  inlined: number;
  enumsMerged: number;
  branchesDeduped: number;
  unionsCollapsed: number;
  openDropped: number;
}

const UNION_KEYS = ["oneOf", "anyOf"] as const;

function stable(value: unknown): string {
  return JSON.stringify(value, (_k, v) => {
    if (!v || typeof v !== "object" || Array.isArray(v)) return v;
    return Object.fromEntries(Object.entries(v as Node).sort(([a], [b]) => a.localeCompare(b)));
  });
}

/** True for `{ type: "string", enum: [...] }` and nothing else: a branch that
 *  can be merged into a sibling enum without changing what validates. */
function isStringEnum(node: unknown): node is Node & { enum: unknown[] } {
  if (!node || typeof node !== "object") return false;
  const n = node as Node;
  const keys = Object.keys(n).filter((k) => k !== "description" && k !== "title");
  return (
    Array.isArray(n.enum) && n.type === "string" && keys.every((k) => k === "type" || k === "enum")
  );
}

/** Whether this union is exactly `T | null` over a `$ref`, ignoring any number
 *  of null branches.
 *
 *  This is the ONE case where keeping the `$ref` is better than inlining it:
 *  typify renders it as `Option<T>` naming the real type, where inlining leaves
 *  it an anonymous object to mint a fresh name for. Every other union is safer
 *  inlined, which is what pass 3 did unconditionally before. */
function isNullableRef(branches: readonly unknown[], refPrefix: string): boolean {
  const nonNull = branches.filter((b) => (b as Node)?.type !== "null");
  if (nonNull.length !== 1 || nonNull.length === branches.length) return false;
  const ref = (nonNull[0] as Node)?.$ref;
  return typeof ref === "string" && ref.startsWith(refPrefix);
}

/**
 * Rewrite `bundle` in place into the dialect typify reads, repeating until
 * nothing changes.
 *
 * The repetition is load-bearing, not defensive. The passes below run per node
 * in a fixed order, but inlining a `$ref` branch splices in a subtree that the
 * earlier passes have already walked past: inlining `WsFrame`'s two branches
 * produces exactly the nested union that pass 2 exists to flatten. One sweep
 * leaves that half-normalised, and typify renders the result as a struct of
 * flattened `Option<SubtypeN>` fields that accepts almost nothing. Iterating to
 * a fixed point is simpler than ordering the passes to be single-sweep
 * complete, and it cannot be quietly wrong.
 *
 * `refPrefix` differs between the two generators: Zod's `z.toJSONSchema()`
 * targets draft 2020-12 and writes `#/$defs/`, ts-json-schema-generator writes
 * draft-07 `#/definitions/`.
 */
export function normalise(bundle: Node, defs: Node, refPrefix: string): Stats {
  const total: Stats = {
    rounds: 0,
    flattened: 0,
    constToEnum: 0,
    inlined: 0,
    enumsMerged: 0,
    branchesDeduped: 0,
    unionsCollapsed: 0,
    openDropped: 0,
  };
  // A bound rather than `while (true)`: a schema that somehow oscillates should
  // fail loudly here, not hang the gate.
  for (let round = 0; round < 16; round++) {
    const stats = sweep(bundle, defs, refPrefix);
    total.rounds++;
    let changed = false;
    for (const key of Object.keys(stats) as Array<keyof Stats>) {
      if (key === "rounds") continue;
      total[key] += stats[key];
      if (stats[key] > 0) changed = true;
    }
    if (!changed) return total;
  }
  throw new Error("normalisation did not converge in 16 rounds");
}

function sweep(bundle: Node, defs: Node, refPrefix: string): Stats {
  const stats: Stats = {
    rounds: 0,
    flattened: 0,
    constToEnum: 0,
    inlined: 0,
    enumsMerged: 0,
    branchesDeduped: 0,
    unionsCollapsed: 0,
    openDropped: 0,
  };

  function walk(node: unknown): void {
    if (Array.isArray(node)) {
      for (const n of node) walk(n);
      return;
    }
    if (!node || typeof node !== "object") return;
    const n = node as Node;

    // Pass 1: `const: x` -> `enum: [x]`. typify does not read `const`, so a
    // discriminant left as `const` is typed `String` and the union loses its
    // tag.
    if ("const" in n && !("enum" in n)) {
      n.enum = [n.const];
      delete n.const;
      stats.constToEnum++;
    }

    for (const key of UNION_KEYS) {
      let branches = n[key];
      if (!Array.isArray(branches)) continue;

      // Pass 2: flatten a union of unions. A TypeScript type alias built from
      // other unions (`WsFrame = ClientToServerFrame | ServerToClientFrame`)
      // nests, and typify does not descend: it sees two opaque branches, finds
      // no shared discriminant, and emits the flattened-Option struct.
      for (let depth = 0; depth < 8; depth++) {
        const next: unknown[] = [];
        let nested = false;
        for (const branch of branches as unknown[]) {
          const inner = (branch as Node)?.[key];
          const only =
            branch && typeof branch === "object" && Object.keys(branch as Node).length === 1;
          if (Array.isArray(inner) && only) {
            next.push(...inner);
            nested = true;
            stats.flattened++;
          } else {
            next.push(branch);
          }
        }
        branches = next;
        if (!nested) break;
      }

      // Pass 3: inline `$ref` branches, but ONLY where something downstream
      // needs to see through the ref.
      //
      // typify cannot see a discriminant through a `$ref`, so a union of refs
      // falls back to untagged even when every target carries the same literal
      // `kind`; and pass 5 below cannot recognise a string enum it has not
      // resolved. Those are the two cases, and they are the whole reason this
      // pass exists.
      //
      // Inlining UNCONDITIONALLY costs a named type, which is not cosmetic.
      // `Record<PresetBucket, BucketRecommendation | null>` makes each property
      // an `anyOf`, and inlining the `$ref` inside it left typify with three
      // anonymous inline objects to name: out came
      // `RecommendationSetBucketsFull`, `...Half` and `...Smallest`, identical
      // to each other and to `BucketRecommendation`. One `fit.rs` function
      // cannot honestly return three types, which is what kept the models
      // package on a hand-written `wire.rs` (D-055). `TtsPresetView.voices`
      // had it too, and the tell is that the `$ref`s in the same schema NOT
      // inside a union survive untouched.
      //
      // So: resolve the branches to decide, and keep the refs when the answer
      // is no. A nullable (`T | null`) is the common no, and it comes back out
      // as `Option<T>` naming the real type.
      if (!isNullableRef(branches as unknown[], refPrefix)) {
        branches = (branches as unknown[]).map((branch) => {
          const ref = (branch as Node)?.$ref;
          if (typeof ref !== "string" || !ref.startsWith(refPrefix)) return branch;
          const target = defs[ref.slice(refPrefix.length)];
          if (!target) return branch;
          stats.inlined++;
          return structuredClone(target);
        });
      }
      // Pass 4: drop structurally identical branches. Flattening a union of
      // unions can bring the same variant in twice (`WsFrame` gets `ping` and
      // `pong` from both directions), and a duplicate variant is a Rust
      // compile error at best and an unreachable arm at worst.
      const seen = new Set<string>();
      branches = (branches as unknown[]).filter((branch) => {
        const key = stable(branch);
        if (seen.has(key)) {
          stats.branchesDeduped++;
          return false;
        }
        seen.add(key);
        return true;
      });

      // Pass 5: merge a union of string enums into one enum. `A | "literal"`
      // where A is itself a string-literal union is common in the domain types
      // (`RequirementGroup | "binary"`), and typify renders the union form as
      // the flattened-Option struct, which then rejects the plain string the
      // wire actually carries.
      const list = branches as unknown[];
      if (list.length > 1 && list.every(isStringEnum)) {
        const values: unknown[] = [];
        for (const branch of list as Array<Node & { enum: unknown[] }>) {
          for (const v of branch.enum) if (!values.includes(v)) values.push(v);
        }
        delete n[key];
        n.type = "string";
        n.enum = values;
        stats.enumsMerged++;
        continue;
      }

      // Pass 6: a union of one is not a union. Left alone, typify wraps it in a
      // pointless newtype.
      if (list.length === 1) {
        delete n[key];
        Object.assign(n, structuredClone(list[0]));
        stats.unionsCollapsed++;
        walk(n);
        continue;
      }

      // Pass 7: an open `additionalProperties` on a union branch makes typify
      // refuse to tag the union AND lose the branch names. Zod 4 writes
      // `.passthrough()` as the empty schema `{}`, not `true`, which is why
      // checking for `true` alone matches nothing. Dropping it trades retention
      // of unknown fields for serde's default ignore-unknown: the same
      // tolerance, without the catch-all map. `.strict()` branches emit
      // `additionalProperties: false` and are left alone.
      for (const branch of list as Node[]) {
        const ap = branch?.additionalProperties;
        const open = ap === true || (ap && typeof ap === "object" && Object.keys(ap).length === 0);
        if (open) {
          delete branch.additionalProperties;
          stats.openDropped++;
        }
      }

      n[key] = list;
    }

    for (const v of Object.values(n)) walk(v);
  }

  walk(bundle);
  return stats;
}

// --- integers ---------------------------------------------------------------
//
// TypeScript has one numeric type, so `ts-json-schema-generator` writes
// `"type": "number"` for a millisecond timestamp exactly as it does for a
// cosine similarity, and typify renders both as `f64`. Serde then puts
// `{"expiresAtMs":1786651789246.0}` on the wire where the TypeScript core puts
// `1786651789246`. Every consumer parses JSON so nothing breaks, but a package
// that wants an integer timestamp writes its own struct instead, and that
// second copy of a wire shape is what drifts. See D-030.
//
// The list below is an ALLOWLIST and must stay one. Declaring a field integer
// that is not one breaks at runtime (`similarity: 0.87` stops round-tripping);
// leaving a field off simply keeps it `f64`. Only the safe direction is
// acceptable, so a field goes in only after its declaration in `@loca/shared`
// and its producer have been read.
//
// Deliberately absent, each verified fractional at its producer:
//   cpuPct      a CPU percentage sample, straight off the OS
//   rssMb       `s.memory / 1024 / 1024`
//   progress    a 0..1 completion fraction
//   score       a relevance score from the embedding index
//   similarity  a cosine similarity
//   speed       a TTS rate multiplier (1.2 is normal)
//   vector      an embedding
//
// Matching is by property NAME across both bundles, which is what makes the
// coverage tripwire in `generate.ts` possible: an entry that matches nothing
// means the contract renamed the field and the declaration silently stopped
// applying. An allowlisted property resolves through an array's `items` and
// through a union's branches before its type is read, so `weekdays: number[]`
// and `ports: (number | "*")[]` are both covered while `vector: number[]` is
// not.
export const INTEGER_FIELDS: ReadonlyMap<string, string> = new Map([
  ["activeStreams", "CoreQueues: count of in-flight streams"],
  ["addedAtMs", "epoch ms, Date.now()"],
  ["atMs", "epoch ms; the Zod ScheduleSpec already declares it .int().positive()"],
  ["availableRamBytes", "byte count from sysinfo; a RAM figure is whole bytes"],
  ["code", "subprocess exit code the extension installer reports (0 or 1)"],
  ["completion", "TokenUsage: completion token count"],
  ["contextSize", "token count for the KV cache; a context window is whole tokens"],
  ["cpuCoresPhysical", "core count from sysinfo"],
  ["createdAtMs", "epoch ms, Date.now()"],
  ["day", "day of month; the Zod ScheduleSpec declares it .int().min(1).max(31)"],
  ["downloadedBytes", "byte count"],
  ["enabledToolCount", "count of enabled tools"],
  ["everyMinutes", "interval in minutes; the Zod ScheduleSpec declares it .int().min(1)"],
  ["expiresAtMs", "epoch ms, Date.now() plus a fixed lifetime"],
  ["fileSizeBytes", "byte count: a catalog quant's on-disk size"],
  ["footprintBytes", "byte count: the fit engine's estimated resident size"],
  ["grantedAtMs", "epoch ms, Date.now()"],
  ["gpuLayers", "count of layers offloaded to the GPU, passed to llama.cpp as -ngl"],
  ["hour", "hour of day; the Zod ScheduleSpec declares it .int().min(0).max(23)"],
  ["idleUnloadSeconds", "whole seconds before an idle model is unloaded"],
  ["installedAtMs", "epoch ms, Date.now()"],
  ["lastSeenMs", "epoch ms, Date.now()"],
  ["llmActive", "CoreQueues: count of running LLM jobs"],
  ["llmQueued", "CoreQueues: count of queued LLM jobs"],
  ["messageCount", "count of messages in a session"],
  ["minute", "minute of hour; the Zod ScheduleSpec declares it .int().min(0).max(59)"],
  ["missingRequired", "indices into Tool.requiredPermissions"],
  ["month", "month of year; the Zod ScheduleSpec declares it .int().min(1).max(12)"],
  ["ord", "message ordinal: an array index, or max+1, with -1 as the pre-store sentinel"],
  ["pid", "OS process id"],
  ["ports", "TCP port; the Zod loca.json schema declares it .int().min(1).max(65535)"],
  ["prompt", "TokenUsage: prompt token count"],
  ["promptCount", "count of prompts an MCP server exposes"],
  ["reasoningBudget", "token cap on thinking, same units as contextSize"],
  ["reasoningDurationMs", "Math.max(0, endMs - startMs), both from Date.now()"],
  ["resourceCount", "count of resources an MCP server exposes"],
  ["size", "Deno.stat().size, and sums of it"],
  ["sizeBytes", "byte count: a stat size, a GitHub asset size, or a Content-Length"],
  ["sizeHint", "byte count, carried through from sizeBytes"],
  ["speechActive", "CoreQueues: count of running speech jobs"],
  ["speechQueued", "CoreQueues: count of queued speech jobs"],
  ["threads", "count of inference threads, passed to llama.cpp as -t"],
  ["toolCount", "count of tools"],
  ["toolsSent", "count of tools exposed to the model this turn"],
  ["topK", "count of entries to keep, used as slice(0, Math.max(0, topK))"],
  ["total", "TokenUsage: total token count"],
  ["totalRamBytes", "byte count from sysinfo; a RAM figure is whole bytes"],
  ["total_size", "sum of StorageCategory.size"],
  ["updatedAtMs", "epoch ms, Date.now()"],
  ["uptimeMs", "Math.floor(performance.now())"],
  ["vramBytes", "byte count of GPU memory reported by the graphics backend"],
  ["weekdays", "day-of-week indices; the Zod ScheduleSpec declares them .int().min(0).max(6)"],
  ["weeklyDownloads", "npm registry download count"],
]);

/** The subschemas an allowlisted property carries its number on: itself for a
 *  plain `number`, an array's element schema for `number[]`, and each branch of
 *  a union for `number | "*"`. */
function targetsOf(schema: Node, out: Node[]): void {
  if (schema.type === "array" && schema.items && typeof schema.items === "object") {
    targetsOf(schema.items as Node, out);
    return;
  }
  for (const key of UNION_KEYS) {
    const branches = schema[key];
    if (!Array.isArray(branches)) continue;
    for (const branch of branches) {
      if (branch && typeof branch === "object") targetsOf(branch as Node, out);
    }
    return;
  }
  out.push(schema);
}

/** Visit every subschema an allowlisted property resolves to, anywhere in the
 *  bundle. */
function eachAllowlisted(node: unknown, visit: (name: string, schema: Node) => void): void {
  if (Array.isArray(node)) {
    for (const n of node) eachAllowlisted(n, visit);
    return;
  }
  if (!node || typeof node !== "object") return;
  const n = node as Node;
  const props = n.properties;
  if (props && typeof props === "object") {
    for (const [name, sub] of Object.entries(props as Node)) {
      if (!INTEGER_FIELDS.has(name) || !sub || typeof sub !== "object") continue;
      const targets: Node[] = [];
      targetsOf(sub as Node, targets);
      for (const target of targets) visit(name, target);
    }
  }
  for (const v of Object.values(n)) eachAllowlisted(v, visit);
}

/** Rewrite every allowlisted `number` to `integer` in place, returning how many
 *  sites each name covered. A name absent from the result covered nothing,
 *  which `generate.ts` treats as a build failure. */
export function declareIntegers(bundle: Node): Map<string, number> {
  const counts = new Map<string, number>();
  eachAllowlisted(bundle, (name, schema) => {
    if (schema.type !== "number") return;
    schema.type = "integer";
    counts.set(name, (counts.get(name) ?? 0) + 1);
  });
  return counts;
}

/** Report the things that would otherwise be found by rustc, minutes later, or
 *  not at all. Returns the number of warnings emitted. */
export function audit(bundle: Node, defs: Node): number {
  let warnings = 0;

  // draft-2020-12 keywords typify's schemars 0.8 model does not read. Present
  // means silently ignored, so the generated type is quietly weaker than the
  // contract.
  const raw = JSON.stringify(bundle);
  for (const kw of ["prefixItems", "$dynamicRef", "unevaluatedProperties", "dependentSchemas"]) {
    if (raw.includes(`"${kw}"`)) {
      console.error(`  WARN unsupported keyword present: ${kw}`);
      warnings++;
    }
  }

  // typify derives names for anonymous nested shapes from their JSON pointer
  // path, and those can collide with an explicit definition name. It then emits
  // the same `pub struct` twice and only rustc notices.
  const seen = new Set<string>();
  for (const k of Object.keys(defs)) {
    const rust = k.replace(/[^A-Za-z0-9]/g, "");
    if (seen.has(rust)) {
      console.error(`  WARN duplicate Rust type name: ${rust}`);
      warnings++;
    }
    seen.add(rust);
  }

  return warnings;
}

export function summary(stats: Stats): string {
  return [
    `${stats.rounds} rounds`,
    `${stats.constToEnum} const->enum`,
    `${stats.flattened} nested unions flattened`,
    `${stats.inlined} refs inlined`,
    `${stats.branchesDeduped} duplicate branches`,
    `${stats.enumsMerged} enum unions merged`,
    `${stats.unionsCollapsed} single-branch unions collapsed`,
    `${stats.openDropped} open additionalProperties dropped`,
  ].join(", ");
}
