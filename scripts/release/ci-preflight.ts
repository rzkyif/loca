#!/usr/bin/env -S deno run -A
// CI preflight: decide what a channel-branch push has to do, and emit two gates.
//
// The Release workflow runs on every push to `latest` / `stable`, but a local
// `deno task release` also pushes those branches after it has already published
// to R2. To keep that local-release push from re-running work, this job diffs
// each release item against the shared R2 release-state cursor (the same change
// detection runReleasePlan uses) and emits two signals to $GITHUB_OUTPUT:
//
//   changed=true|false  any item differs -> the publish job runs.
//   native=true|false   a matrix-built item (core / desktop client / android)
//                        differs -> the expensive per-triple build matrix runs.
//
// The platform-independent items (extension, catalog, schemas,
// the landing page) carry no per-triple artifact and build directly on the
// publish coordinator, so a push that touches ONLY them (e.g. a website-only
// change) publishes without spinning up a single build runner: changed=true,
// native=false, build skipped, publish runs just the changed items.
//
// Reads only the public R2 cursor + live manifests, so it needs no deploy
// credentials; envFromProcess just wants the (non-secret) Ed25519 PUBLIC key.
//
// Flags:
//   --channel=stable|latest   target channel (the workflow sets it from the branch)

import { parseArgs } from "@std/cli/parse-args";
import {
  colors,
  envFromProcess,
  fail,
  hasReleaseChanges,
  ok,
  parseChannelFlag,
  type ReleaseItem,
  step,
} from "./lib.ts";
import { coreItem } from "./core.ts";
import { extensionItem } from "./extension.ts";
import { catalogItem } from "./catalog.ts";
import { clientItem } from "./client.ts";
import { androidItem } from "./android.ts";
import { schemasItem } from "./schemas.ts";
import { websiteItem } from "./website.ts";

// Matrix-built items: each carries a per-triple artifact produced by a native
// build runner, so any change here requires the build matrix. This is the set
// the `build` job gates on.
const NATIVE_ITEMS: ReleaseItem[] = [coreItem, clientItem, androidItem];

// The full set the umbrella release (main.ts) and ci-publish plan against: the
// matrix-built items plus the platform-independent ones that build on the
// publish coordinator. Any change here means the publish job has work to do.
const ITEMS: ReleaseItem[] = [
  ...NATIVE_ITEMS,
  extensionItem,
  catalogItem,
  schemasItem,
  websiteItem,
];

async function main(): Promise<void> {
  const args = parseArgs(
    Deno.args.filter((a) => a !== "--"),
    { string: ["channel"] },
  );
  const channel = parseChannelFlag(args.channel);
  console.log(colors.bold(`\nloca ci-preflight: ${channel} channel\n`));

  step("Diffing release items against the R2 cursor");
  const env = envFromProcess();
  const changed = await hasReleaseChanges(env, ITEMS, channel);
  const native = changed && (await hasReleaseChanges(env, NATIVE_ITEMS, channel));
  ok(
    !changed
      ? "nothing changed; build + publish will be skipped"
      : native
        ? "native changes detected; build + publish will run"
        : "only platform-independent items changed; build skipped, publish will run",
  );

  const outFile = Deno.env.get("GITHUB_OUTPUT");
  const lines = `changed=${changed}\nnative=${native}\n`;
  if (outFile) await Deno.writeTextFile(outFile, lines, { append: true });
  else console.log(lines.trimEnd());
}

if (import.meta.main) {
  try {
    await main();
  } catch (err) {
    fail(err instanceof Error ? err.message : String(err));
  }
}
