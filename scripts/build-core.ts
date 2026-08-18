// Builds the loca-core binary (the Rust crate `loca-core-rs`) + the one
// surviving native helper crate (updater) for one or more triples, into
// dist/<triple>/ with channel-suffixed names.
//
// Thin wrapper over the same buildAll / buildHelpers the release uses, so there
// is a single core-build implementation and a local build is what ships.
//
// The core is built with `local-inference` and `local-speech`, which are OFF by
// default in the crate for CI's sake and must be ON in anything shipped. The
// llama.cpp runtime libraries land in dist/<triple>/ beside the binary and are
// not optional: under GGML_BACKEND_DL every backend is a loadable module, the
// CPU one included, so a core without them registers zero compute devices.
//
// Default target is the current host. Pass `--target=<triple>` (repeatable, or
// comma-separated) to override; cargo cross-compilation for non-host triples
// needs the matching Rust target installed (`rustup target add <triple>`).
//
// Flags:
//   --channel=stable|latest   channel suffix on our binary names (default stable)
//   --target=<triple>         triple(s) to build; repeatable/csv (default host)

import { parseArgs } from "@std/cli/parse-args";
import type { Triple } from "../packages/loca-shared/src/domain/model.ts";
import { ALL_TRIPLES, buildAll, buildHelpers } from "./release/core.ts";
import { channelBinSuffix, fail, parseChannelFlag } from "./release/lib.ts";

const args = parseArgs(Deno.args, { string: ["target", "channel"], collect: ["target"] });
const channel = parseChannelFlag(args.channel);
const suffix = channelBinSuffix(channel);

const rawTargets = (args.target as string[]).flatMap((t) => t.split(","));
const triples = (rawTargets.length > 0 ? rawTargets : [Deno.build.target]).map((t) => t.trim());
for (const t of triples) {
  if (!(ALL_TRIPLES as readonly string[]).includes(t)) {
    fail(`unknown --target "${t}". Valid: ${ALL_TRIPLES.join(", ")}`);
  }
}

await buildAll(triples as Triple[], suffix);
await buildHelpers(triples as Triple[], suffix);
console.log(`done. artifacts at dist/{${triples.join(",")}}`);
