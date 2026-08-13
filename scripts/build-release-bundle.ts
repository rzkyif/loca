// In-environment build entry for the all-targets release.
//
// Runs INSIDE a single build environment (the host, a Podman Linux container, or
// the Windows VM) and produces that environment's share of a release: it builds
// the requested artifacts into dist/<triple>/ and writes a descriptor (the
// ArtifactBundle / ClientDescriptor contract in scripts/release/artifacts.ts)
// describing them. The orchestrator on the host collects each environment's dist
// subtree + descriptor, then composes + signs + uploads the unified manifests once.
//
// It has NO R2/Cloudflare/Ed25519-PRIVATE-key dependency: signing and upload stay
// on the host. The only secrets an environment needs are the Tauri minisign key
// (desktop client installer .sig) - injected via the process env - and never the
// trust-root private key.
//
// Flags:
//   --kind=core|client        what to build (default core)
//   --channel=stable|latest   channel (default stable)
//   --target=<triple>         triple to build; repeatable, or comma-separated.
//                             `client` builds exactly one triple per invocation.
//   --bundles=<csv>           client only: narrow the Tauri bundle targets (the
//                             cross-built Linux client passes `appimage`).
//   --bundle-dir=<dir>        dir to write the descriptor into (default dist/)

import { parseArgs } from "@std/cli/parse-args";
import { ensureDir } from "@std/fs/ensure-dir";
import type { Triple } from "../packages/loca-shared/src/domain/model.ts";
import { bundleCoreArtifacts, writeBundle, writeClientDescriptor } from "./release/artifacts.ts";
import { ALL_TRIPLES, buildCoreArtifacts } from "./release/core.ts";
import { buildClientBundle } from "./release/client.ts";
import {
  channelBinSuffix,
  DIST_DIR,
  envFromProcess,
  fail,
  parseChannelFlag,
  readCoreVersion,
  rel,
} from "./release/lib.ts";

const KINDS = ["core", "client"] as const;
type Kind = (typeof KINDS)[number];

const args = parseArgs(Deno.args, {
  string: ["kind", "channel", "target", "bundle-dir", "bundles"],
  collect: ["target"],
});

const kind = (args.kind ?? "core") as Kind;
if (!(KINDS as readonly string[]).includes(kind)) {
  fail(`unknown --kind "${kind}". Valid: ${KINDS.join(", ")}`);
}

const channel = parseChannelFlag(args.channel);

// `--target=a,b --target=c` and `--target=a --target=b` both work; default host.
const rawTargets = (args.target as string[]).flatMap((t) => t.split(","));
const triples = (rawTargets.length > 0 ? rawTargets : [Deno.build.target]).map((t) => t.trim());
for (const t of triples) {
  if (!(ALL_TRIPLES as readonly string[]).includes(t)) {
    fail(`unknown --target "${t}". Valid: ${ALL_TRIPLES.join(", ")}`);
  }
}

const bundleDir = args["bundle-dir"] ?? DIST_DIR;
await ensureDir(bundleDir);

if (kind === "core") {
  const suffix = channelBinSuffix(channel);
  const version = await readCoreVersion();
  const built = await buildCoreArtifacts(triples as Triple[], suffix);
  const bundle = bundleCoreArtifacts(built, version, channel, triples as Triple[]);
  const bundlePath = await writeBundle(bundleDir, bundle);
  console.log(
    `done. ${bundle.records.length} core artifacts under dist/, bundle at ${rel(bundlePath)}`,
  );
} else {
  // One triple per client invocation: each arch needs its own Tauri build (and,
  // in the Windows VM, its own vcvars arch), so the driver loops and gives each a
  // dedicated bundle-dir.
  if (triples.length !== 1) fail(`--kind=client builds one --target per invocation`);
  const env = envFromProcess();
  const bundles = (args.bundles ?? "")
    .split(",")
    .map((b) => b.trim())
    .filter(Boolean);
  const descriptor = await buildClientBundle(env, channel, {
    triple: triples[0] as Triple,
    bundles: bundles.length ? bundles : undefined,
  });
  const descriptorPath = await writeClientDescriptor(bundleDir, descriptor);
  console.log(
    `done. client ${descriptor.filename} under dist/, descriptor at ${rel(descriptorPath)}`,
  );
}
