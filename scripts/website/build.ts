#!/usr/bin/env -S deno run -A
// Builds the Astro site to packages/loca-website/dist/. No idempotency
// probe, no R2 calls, no wrangler. Use `deno task release` for the
// full deploy pipeline.

import { astroBuild } from "../release/lib.ts";

await astroBuild();
