// Builds the loca-core binary + the four native helper crates (updater,
// keychain, hwinfo, ptyhost) for one or more triples, into dist/<triple>/ with
// channel-suffixed names.
//
// Thin wrapper over the same buildAll / buildHelpers the release uses, so there
// is a single core-build implementation: the lean compile workspace (a ~94 MB
// binary instead of ~2.2 GB) and all four helpers, identical to what ships.
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
