# The contract bridge

Generates the Rust wire types in `packages/loca-core-rs/src/contract/` from
`@loca/shared`, which stays the single source of truth for the contract.

```
deno task contract         regenerate; commit the result
deno task contract:check   fail if the checked-in output is stale (runs in `deno task check`)
```

The staleness check is in the gate because a stale generated file is otherwise
invisible: the Rust compiles fine against last week's contract.

## Why two generators

The contract has two homes in `@loca/shared`, and they are not duplicates of
each other. They are a producer side and a consumer side with deliberately
different strictness. Generating everything from one of them loses something
real either way, so the split is by **direction of travel**:

| output       | from                                   | covers                                                                                        |
| ------------ | -------------------------------------- | --------------------------------------------------------------------------------------------- |
| `inbound.rs` | Zod (`validation/*.ts`)                | everything the core validates on the way in                                                   |
| `api.rs`     | TypeScript (`api/*.ts`, `domain/*.ts`) | everything else: responses, the frames the core produces, the domain shapes both sides render |

Zod earns its half because typify carries `.min` / `.max` / `.int` / `.regex`
into a `Deserialize` that enforces them. **Deserializing an `inbound` type IS
the validation**; there is no second check to write. TypeScript cannot express
any of those, and `number` degrades to `f64` where Zod gives `i64`.

TypeScript earns its half because it is the only place those shapes exist, and
because the payloads matter: the server-to-client Zod union deliberately types
`message` as `z.unknown()` (it is the client's envelope parser, and the real
typing is owned downstream). Generating the core's outbound frame type from it
would produce a frame that cannot carry a `Message` or a `Session`. See D-006.

## Files

| file               | what                                                                                                                   |
| ------------------ | ---------------------------------------------------------------------------------------------------------------------- |
| `generate.ts`      | the driver: emits both bundles plus the fixtures, runs codegen, writes or verifies                                     |
| `emit-zod.ts`      | Zod half: `z.toJSONSchema()` over the validators                                                                       |
| `emit-ts.ts`       | TypeScript half: `ts-json-schema-generator` over `ts-entry.ts`                                                         |
| `normalise.ts`     | the shared passes that turn either output into the dialect typify reads                                                |
| `emit-fixtures.ts` | round-trip fixtures, each validated by its own Zod schema first                                                        |
| `codegen/`         | a small Rust tool wrapping typify; excluded from the root workspace so its dependencies never touch the shipped crates |

## The traps

Every one of these degrades the output **silently**. The Rust still compiles and
the types still deserialize something; the defect surfaces later as a mystery
`data did not match any variant`, or as a struct that accepts almost nothing.
`normalise.ts` documents each pass at its implementation; the short version:

- `const: x` is not read by typify, so a discriminant left as `const` types as
  `String` and the union loses its tag.
- A `$ref` branch hides the discriminant, so a union of refs falls back to
  untagged even when every target carries the same literal `kind`.
- An open `additionalProperties` on a branch makes typify refuse to tag the
  union **and** lose the branch names. Zod 4 writes `.passthrough()` as the empty
  schema `{}`, not `true`, so checking for `true` matches nothing.
- A union of unions (`WsFrame = ClientToServerFrame | ServerToClientFrame`) is
  not flattened by typify: it sees two opaque branches and emits a struct of
  `#[serde(flatten)] Option<SubtypeN>` fields.
- A union of string enums (`RequirementGroup | "binary"`) gets the same
  flattened-struct treatment, which then rejects the plain string the wire
  actually carries.
- Converting `anyOf` to `oneOf` looks like it belongs in this list and does not.
  It made the result strictly worse in the S0.3 spike.

The passes run to a **fixed point**, which is load-bearing: inlining a `$ref`
splices in a subtree the earlier passes have already walked past.

Two more that no pass can catch, so the emitters warn instead: typify derives
names for anonymous nested shapes from their JSON pointer path and can collide
with an explicit definition name (rustc catches it, minutes later), and a
self-referential `$ref` sends typify into a stack overflow rather than an error.

## Adding a type

Change the TypeScript or the Zod schema in `@loca/shared`, then
`deno task contract`, then commit the generated files. Do not edit the generated
files. If a shape needs a Rust name it does not have, name it on the Zod side
with `.meta({ id })` or rename the export.

If you add a case to `emit-fixtures.ts`, add the matching arm to
`packages/loca-core-rs/tests/contract_roundtrip.rs`; the test panics on an
unknown fixture type rather than skipping it.
