// Emits the JSON Schema bundle for the TypeScript half of the wire contract:
// everything in `api/` and `domain/` that has no Zod schema, which is roughly
// 89% of the contract's types.
//
// These are the shapes the core PRODUCES (responses, server->client frames) and
// the domain shapes both sides render. They cross no trust boundary on the way
// in, so nothing is lost by generating them from TypeScript, which cannot
// express a constraint anyway.
//
// Run via `deno task contract`, not directly.

import { dirname, fromFileUrl, join } from "@std/path";

import { audit, type Node, normalise, summary } from "./normalise.ts";

const HERE = dirname(fromFileUrl(import.meta.url));

const out = Deno.args[0];
if (!out) {
  console.error("usage: emit-ts.ts <out.json>");
  Deno.exit(2);
}

// `-t '*'` takes every exported type in the program, so the domain modules come
// along transitively; `-e export` keeps unexported internals out. JSDoc becomes
// `description`, which typify turns into doc comments on the generated types.
const gen = new Deno.Command(Deno.execPath(), {
  args: [
    "run",
    "-A",
    "npm:ts-json-schema-generator@2.4.0",
    "--path",
    join(HERE, "ts-entry.ts"),
    "--tsconfig",
    join(HERE, "tsconfig.json"),
    "--type",
    "*",
    "--expose",
    "export",
    "--no-type-check",
  ],
  stdout: "piped",
  stderr: "piped",
});
const result = await gen.output();
if (!result.success) {
  console.error(new TextDecoder().decode(result.stderr));
  Deno.exit(1);
}

const bundle = JSON.parse(new TextDecoder().decode(result.stdout)) as Node;

// Draft-07 names the map `definitions`; typify's schemars 0.8 model reads that
// key, so it is left as-is. The root `$ref` would make typify emit one alias
// for the entry module, which is noise.
delete bundle.$ref;
const definitions = (bundle.definitions ?? {}) as Node;

const stats = normalise(bundle, definitions, "#/definitions/");
const warnings = audit(bundle, definitions);

await Deno.writeTextFile(out, JSON.stringify(bundle, null, 2));
console.log(
  `ts:  ${Object.keys(definitions).length} defs ` +
    `(${summary(stats)}${warnings ? `, ${warnings} warnings` : ""})`,
);
