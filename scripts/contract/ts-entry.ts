// The TypeScript half of the wire contract, as one program.
//
// `ts-json-schema-generator` walks a single entry point, so re-exporting the
// api surface here gives it one program and one `definitions` map with shared
// types deduplicated.
//
// Consumed by `emit-ts.ts`; not part of any runtime build.

export type * from "../../packages/loca-shared/src/api/http.ts";
export type * from "../../packages/loca-shared/src/api/ws.ts";
export type * from "../../packages/loca-shared/src/api/errors.ts";

// The pairing RESPONSE shapes. They live in `api/` rather than beside the
// request validators precisely so this generator can reach them: a module full
// of `z.infer<typeof ...>` aliases makes ts-json-schema-generator fall over
// resolving zod's internals, and without these four the core would hand-write
// them, which is a second source of truth for a wire shape.
export type * from "../../packages/loca-shared/src/api/pairing.ts";

// Domain modules the api surface does not reach.
//
// Most of `domain/` arrives transitively, because an api type references it.
// These three are referenced only from route handlers, so nothing pulled them
// in, and "reachable by accident" is not a property to rely on: three separate
// packages independently hand-wrote copies of these 25 types before the gap was
// found, which is precisely the second source of truth the bridge exists to
// prevent. Listing them explicitly makes reachability a decision rather than a
// side effect of someone's import.
export type * from "../../packages/loca-shared/src/domain/mcp.ts";
export type * from "../../packages/loca-shared/src/domain/memory.ts";
export type * from "../../packages/loca-shared/src/domain/recommend.ts";
