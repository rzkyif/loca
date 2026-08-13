#!/usr/bin/env -S deno run -A
// Remote release: promote a channel by moving a REMOTE branch, letting the
// branch-driven CI workflow (.github/workflows/release.yml) build + publish.
//
//   deno task remote-release          latest <- main   (publishes the latest channel)
//   deno task remote-release:stable   stable <- latest (publishes the stable channel)
//
// This is purely remote: it does not read the local working tree, HEAD, or
// branch position. It fast-forwards the remote target branch onto the remote
// source branch via the GitHub refs API, refuses a non-fast-forward, and no-ops
// when the target is already aligned (so it never triggers a redundant CI run).
//
// Flags:
//   --channel=stable|latest   target channel (the deno tasks set this)
//   --help

import { parseArgs } from "@std/cli/parse-args";
import { colors, fail, info, parseChannelFlag } from "./lib.ts";
import { fastForwardRemote, remoteTransfer } from "./git-align.ts";

async function main(): Promise<void> {
  const args = parseArgs(
    Deno.args.filter((a) => a !== "--"),
    { string: ["channel"], boolean: ["help"], default: { help: false } },
  );
  if (args.help) {
    console.log(`Usage: deno task remote-release[:stable]

Moves the remote channel branch (latest <- main, stable <- latest) so CI builds
and publishes that channel. Refuses a non-fast-forward; no-ops when already
aligned. Does not touch the local working tree.`);
    Deno.exit(0);
  }

  const channel = parseChannelFlag(args.channel);
  const { source, target } = remoteTransfer(channel);
  console.log(colors.bold(`\nloca remote-release: ${channel} channel (${target} <- ${source})\n`));

  await fastForwardRemote(channel);

  info(
    `If the branch moved, the "Release" workflow is now building the ${channel} ` +
      `channel. Track it under the repo's Actions tab.`,
  );
}

if (import.meta.main) {
  try {
    await main();
  } catch (err) {
    fail(err instanceof Error ? err.message : String(err));
  }
}
