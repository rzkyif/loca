// Regenerates and verifies `packages/loca-core-rs/data/espeak-ng-data.tar.gz`,
// the Kokoro phonemizer data the Rust core embeds with `include_bytes!` under
// the `local-speech` feature and extracts on first local synthesis.
//
// **Why this exists.** The archive is a committed 8.6 MB binary, and a committed
// binary with no script that regenerates it is not reproducible: its provenance
// lives only in a doc comment, and nobody can check that what is on disk is what
// upstream published. That is the same gap `scripts/vendor-llama.ts` closes for
// the llama.cpp tree.
//
// **What is verified, and what deliberately is NOT.** The upstream asset is
// pinned by sha256 and checked byte for byte before anything is unpacked, so the
// provenance chain is exact. The COMMITTED archive is then checked by CONTENT
// against it (every path, every file's sha256, both directions) rather than by
// byte equality.
//
// That is not a weaker check dressed up, it is the only honest one available:
// tar records mtimes, uid, gid and entry order, and gzip its own mtime and
// compression level, so re-packing the same bytes on a different machine
// produces a different file. Byte equality would fail for reasons unrelated to
// the data; "does this archive contain precisely the pinned upstream data" is
// the right question, and this answers it.
//
//   deno run -A scripts/vendor-espeak.ts           verify the committed archive
//   deno run -A scripts/vendor-espeak.ts --write   regenerate it from upstream
//
// `--write` is for a legitimate upstream bump. It rewrites the archive and
// prints the new sha256 for `ESPEAK_DATA_SHA256_GZ` in `engine.rs`.

import { dirname, fromFileUrl, join } from "@std/path";
import { encodeHex } from "@std/encoding/hex";

/** The upstream asset. MUTABLE by construction (a rolling release tag), which is
 *  why the sha256 below is the actual pin and this URL is only where to look. */
const UPSTREAM_URL =
  "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/espeak-ng-data.tar.bz2";

/** The pinned upstream sha256. */
const UPSTREAM_SHA256 = "4135ccf82e1f40613491c0874d4945ae9e9c7840933d8e25a6f9e003d9ebf533";

const REPO = dirname(dirname(fromFileUrl(import.meta.url)));
const ARCHIVE = join(REPO, "packages/loca-core-rs/data/espeak-ng-data.tar.gz");

const write = Deno.args.includes("--write");

async function sha256Of(bytes: Uint8Array): Promise<string> {
  return encodeHex(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)));
}

function fail(msg: string): never {
  console.error(`vendor-espeak: ${msg}`);
  Deno.exit(1);
}

/** Every archive entry, as `path -> sha256(contents)`. Directories are recorded
 *  with a null digest so a directory becoming a file is visible rather than
 *  silently matching. */
async function manifest(dir: string): Promise<Map<string, string | null>> {
  const out = new Map<string, string | null>();
  async function walk(rel: string) {
    for await (const e of Deno.readDir(join(dir, rel))) {
      const path = rel ? `${rel}/${e.name}` : e.name;
      if (e.isDirectory) {
        out.set(path, null);
        await walk(path);
      } else {
        out.set(path, await sha256Of(await Deno.readFile(join(dir, path))));
      }
    }
  }
  await walk("");
  return out;
}

/** Unpack `archive` (relative name, inside `cwd`) after refusing any entry that
 *  would escape the extraction directory.
 *
 *  Relative names and a set `cwd` only, never an absolute path: the Windows
 *  runner's git-bundled GNU tar reads a drive-letter colon as a remote
 *  `host:file` and breaks `-C` on backslashes. `scripts/release/core.ts` carries
 *  the same constraint for the same reason. */
async function safeUnpack(cwd: string, archive: string, flags: string): Promise<void> {
  const list = await new Deno.Command("tar", {
    args: [`-t${flags}f`, archive],
    cwd,
    stdout: "piped",
    stderr: "inherit",
  }).output();
  if (!list.success) fail(`tar -t${flags}f ${archive} failed`);
  for (const line of new TextDecoder().decode(list.stdout).split("\n")) {
    const name = line.trim();
    if (!name) continue;
    if (name.startsWith("/") || name.split("/").some((s) => s === "..")) {
      fail(`archive entry escapes its directory: ${name}`);
    }
  }
  const x = await new Deno.Command("tar", {
    args: [`-x${flags}f`, archive],
    cwd,
    stderr: "inherit",
  }).output();
  if (!x.success) fail(`tar -x${flags}f ${archive} failed`);
}

const tmp = await Deno.makeTempDir({ prefix: "loca-espeak-" });

// 1. Fetch upstream and verify the pin BEFORE unpacking. An asset swap or a
//    MITM must not propagate into a Loca-signed binary.
console.log("fetching the pinned upstream archive");
const res = await fetch(UPSTREAM_URL);
if (!res.ok) fail(`fetch failed: ${res.status} ${res.statusText}`);
const upstreamBytes = new Uint8Array(await res.arrayBuffer());
const upstreamSha = await sha256Of(upstreamBytes);
if (upstreamSha !== UPSTREAM_SHA256) {
  fail(
    `upstream sha256 mismatch: pinned ${UPSTREAM_SHA256}, got ${upstreamSha}. ` +
      `If upstream legitimately changed the asset, update UPSTREAM_SHA256 here ` +
      `AND ESPEAK_DATA_SHA256 in scripts/release/core.ts, then re-run with --write.`,
  );
}
const upstreamDir = join(tmp, "upstream");
await Deno.mkdir(upstreamDir);
await Deno.writeFile(join(upstreamDir, "u.tar.bz2"), upstreamBytes);
await safeUnpack(upstreamDir, "u.tar.bz2", "j");
await Deno.remove(join(upstreamDir, "u.tar.bz2"));

if (write) {
  // gzip, not bzip2: the crate already depends on `flate2` + `tar`, and adding a
  // bzip2 decoder for one startup path is not worth the dependency.
  const pack = await new Deno.Command("tar", {
    args: ["-czf", "out.tar.gz", "espeak-ng-data"],
    cwd: upstreamDir,
    stderr: "inherit",
  }).output();
  if (!pack.success) fail("repack failed");
  const packed = await Deno.readFile(join(upstreamDir, "out.tar.gz"));
  await Deno.writeFile(ARCHIVE, packed);
  console.log(`wrote ${ARCHIVE}`);
  console.log(`sha256 (for ESPEAK_DATA_SHA256_GZ in engine.rs): ${await sha256Of(packed)}`);
  Deno.exit(0);
}

// 2. Unpack the COMMITTED archive and compare by content.
let committed: Uint8Array;
try {
  committed = await Deno.readFile(ARCHIVE);
} catch {
  fail(`${ARCHIVE} is missing. Run with --write to generate it.`);
}
const committedDir = join(tmp, "committed");
await Deno.mkdir(committedDir);
await Deno.writeFile(join(committedDir, "c.tar.gz"), committed);
await safeUnpack(committedDir, "c.tar.gz", "z");
await Deno.remove(join(committedDir, "c.tar.gz"));

const want = await manifest(upstreamDir);
const got = await manifest(committedDir);

const problems: string[] = [];
for (const [path, digest] of want) {
  if (!got.has(path)) problems.push(`missing from the committed archive: ${path}`);
  else if (got.get(path) !== digest) problems.push(`content differs: ${path}`);
}
for (const path of got.keys()) {
  if (!want.has(path)) problems.push(`present in the committed archive but not upstream: ${path}`);
}

if (problems.length) {
  for (const p of problems.slice(0, 20)) console.error(`  ${p}`);
  if (problems.length > 20) console.error(`  ... and ${problems.length - 20} more`);
  fail(`${problems.length} difference(s) between upstream and the committed archive`);
}

// Both directions matched, so a truncated or empty extraction cannot pass: an
// empty `want` would mean the upstream archive unpacked to nothing, which the
// entry count below makes visible rather than reporting as success.
if (want.size === 0) fail("upstream unpacked to zero entries; the comparison proved nothing");

await Deno.remove(tmp, { recursive: true });
console.log(
  `ok: the committed archive matches the pinned upstream data exactly ` +
    `(${want.size} entries, upstream sha256 ${UPSTREAM_SHA256.slice(0, 12)}...)`,
);
