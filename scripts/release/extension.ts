// Release item: the built-in extension, packed into a gzipped tarball with a
// signed extension.json manifest (whole-manifest-minus-signature, like
// core.json). Versioned via packages/loca-extension-builtin/deno.json. The
// dev-only samples extension is never released; it has no release item.

import { ensureDir } from "@std/fs/ensure-dir";
import { walk } from "@std/fs/walk";
import { dirname, join, relative } from "@std/path";
import { TarStream, type TarStreamInput } from "@std/tar";
import type { BuiltinExtensionManifest } from "../../packages/loca-shared/src/domain/model.ts";
import { BUILTIN_EXTENSION_ID } from "../../packages/loca-shared/src/domain/extension.ts";
import {
  type ApplyOpts,
  bumpVersionField,
  channelManifestDir,
  channelStoragePrefix,
  colors,
  type DeployEnv,
  DIST_DIR,
  hashPaths,
  humanBytes,
  info,
  ok,
  r2Put,
  readVersionField,
  rel,
  type ReleaseChannel,
  type ReleaseItem,
  REPO_ROOT,
  sha256File,
  signEd25519,
  step,
  stripJsonVersion,
} from "./lib.ts";

const PKG_DIR = join(REPO_ROOT, "packages/loca-extension-builtin");
const MANIFEST_CACHE_CONTROL = "public, max-age=300";
// Dev / VCS / test cruft, plus the lockfile (deno regenerates it on install).
const EXCLUDE_DIRS = new Set(["node_modules", ".git", "tests"]);
const EXCLUDE_FILES = new Set(["deno.lock"]);

/** True when a packed path (relative to PKG_DIR) is excluded from the extension
 *  archive. Shared by buildTarball and sourceHash so the two never disagree. */
function isExcluded(relPath: string): boolean {
  const segs = relPath.split("/");
  if (segs.some((s) => EXCLUDE_DIRS.has(s))) return true;
  if (EXCLUDE_FILES.has(relPath)) return true;
  return relPath.endsWith(".test.ts");
}

function readExtensionVersion(): Promise<string> {
  return readVersionField(join(PKG_DIR, "deno.json"));
}

/** Pack the extension into a gzipped tarball with entries at the root (no
 *  `package/` prefix), so the core installer extracts it directly. Files are
 *  sorted for a deterministic archive. */
async function buildTarball(outPath: string): Promise<void> {
  const files: { rel: string; abs: string; size: number }[] = [];
  for await (const entry of walk(PKG_DIR, { includeDirs: false, includeFiles: true })) {
    const relPath = relative(PKG_DIR, entry.path);
    if (isExcluded(relPath)) continue;
    files.push({
      rel: relPath,
      abs: entry.path,
      size: (await Deno.stat(entry.path)).size,
    });
  }
  files.sort((a, b) => a.rel.localeCompare(b.rel));

  const inputs: TarStreamInput[] = [];
  for (const f of files) {
    inputs.push({
      type: "file",
      path: f.rel,
      size: f.size,
      readable: (await Deno.open(f.abs)).readable,
    });
  }

  await ensureDir(dirname(outPath));
  const out = await Deno.open(outPath, {
    create: true,
    write: true,
    truncate: true,
  });
  await ReadableStream.from(inputs)
    .pipeThrough(new TarStream())
    .pipeThrough(new CompressionStream("gzip"))
    .pipeTo(out.writable);
}

export const extensionItem: ReleaseItem = {
  id: "extension",
  label: "built-in extension",
  scope: "channel",
  packages: ["extension"],
  bumpHint: "packages/loca-extension-builtin/deno.json (version)",

  version: readExtensionVersion,
  versionFile: join(PKG_DIR, "deno.json"),
  bumpVersion: () => bumpVersionField(join(PKG_DIR, "deno.json")),

  sourceHash(_channel: ReleaseChannel): Promise<string> {
    const pkgRel = relative(REPO_ROOT, PKG_DIR) + "/";
    // deno.json carries the extension version; drop it from the tree hash and fold
    // it back in with the version blanked, so a lone bump does not re-trigger an
    // extension release while a real deno.json change still does.
    return hashPaths([
      {
        path: PKG_DIR,
        exclude: (r) => {
          const rp = r.startsWith(pkgRel) ? r.slice(pkgRel.length) : r;
          return rp === "deno.json" || isExcluded(rp);
        },
      },
      { path: join(PKG_DIR, "deno.json"), transform: stripJsonVersion },
    ]);
  },

  async apply(env: DeployEnv, channel: ReleaseChannel, opts: ApplyOpts): Promise<void> {
    const prefix = channelStoragePrefix(channel);
    const manifestDir = channelManifestDir(channel);
    const version = await readExtensionVersion();

    step("Packing tarball");
    const tgzName = `${BUILTIN_EXTENSION_ID}-${version}.tgz`;
    const tgzPath = join(DIST_DIR, manifestDir, "extension", tgzName);
    await buildTarball(tgzPath);
    const { sha256, size } = await sha256File(tgzPath);
    ok(`${rel(tgzPath)}  ${humanBytes(size)}  ${sha256.slice(0, 12)}…`);

    step("Composing + signing extension.json");
    const tarballKey = `${prefix}${version}/extension/${tgzName}`;
    const unsigned: Omit<BuiltinExtensionManifest, "signature"> = {
      schemaVersion: 1,
      version,
      id: BUILTIN_EXTENSION_ID,
      tarballUrl: `https://${env.storageDomain}/${tarballKey}`,
      sha256,
    };
    const manifest: BuiltinExtensionManifest = {
      ...unsigned,
      signature: await signEd25519(env.signingPrivateKey, unsigned),
    };
    const manifestPath = join(DIST_DIR, manifestDir, "extension.json");
    await ensureDir(join(DIST_DIR, manifestDir));
    await Deno.writeTextFile(manifestPath, JSON.stringify(manifest, null, 2));
    ok(`signed extension.json → ${rel(manifestPath)}`);

    if (opts.dryRun) {
      info(colors.yellow(`dry-run: skipping upload of ${manifestDir}/extension.json`));
      return;
    }

    step(`Uploading to R2 bucket "${env.r2Bucket}"`);
    info(`uploading ${tarballKey}  (${humanBytes(size)})`);
    await r2Put(env, tarballKey, tgzPath, "application/gzip");
    opts.recordVersionedKey?.(tarballKey);
    await r2Put(
      env,
      `${manifestDir}/extension.json`,
      manifestPath,
      "application/json",
      MANIFEST_CACHE_CONTROL,
    );
    ok(`https://${env.storageDomain}/${manifestDir}/extension.json`);
  },
};
