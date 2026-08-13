// Release item: the Astro landing page + its Cloudflare Worker. Channel-
// independent (one site serves every channel). sourceHash() hashes the website
// source tree; apply() builds Astro and deploys via wrangler. Idempotency is
// handled upstream by the release-state cursor, so there is no self-served
// release-state.json.
//
// The landing page is a platform-independent item, like the model catalog: it
// carries no per-triple artifact, so it needs no build-matrix runner. It builds
// + deploys directly on the umbrella release's host (the local `deno task
// release`, or the CI publish coordinator), diffed against the same cursor +
// version-bump gate every other item uses. wrangler authenticates with the
// CLOUDFLARE_* credentials from .env (see wranglerEnv in lib.ts).

import { join } from "@std/path";
import {
  type ApplyOpts,
  astroBuild,
  bumpVersionField,
  colors,
  type DeployEnv,
  hashWebsiteSource,
  info,
  ok,
  readVersionField,
  type ReleaseChannel,
  type ReleaseItem,
  step,
  WEBSITE_DIR,
  wranglerDeploy,
} from "./lib.ts";

export const websiteItem: ReleaseItem = {
  id: "website",
  label: "landing page",
  scope: "shared",
  packages: ["website", "shared"],
  bumpHint: "packages/tomat-website/deno.json (version)",

  version: () => readVersionField(join(WEBSITE_DIR, "deno.json")),
  versionFile: join(WEBSITE_DIR, "deno.json"),
  bumpVersion: () => bumpVersionField(join(WEBSITE_DIR, "deno.json")),

  sourceHash(_channel: ReleaseChannel): Promise<string> {
    return hashWebsiteSource();
  },

  // No buildOutputs: like the extension, catalog, and schemas, the
  // landing page is a coordinator-only item with no entry in the unified
  // `deno task build`. Its apply() runs the Astro build itself at release time.

  async apply(env: DeployEnv, _channel: ReleaseChannel, opts: ApplyOpts): Promise<void> {
    step("Building Astro site");
    await astroBuild();

    if (opts.dryRun) {
      info(colors.yellow("dry-run: skipping wrangler deploy"));
      return;
    }

    step("Deploying Worker via wrangler");
    await wranglerDeploy(env);
    ok(`https://${env.websiteDomain}/`);
  },
};
