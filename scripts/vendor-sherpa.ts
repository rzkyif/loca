// Fetches the pinned sherpa-onnx MOBILE prebuilt libraries into
// `vendor/sherpa-onnx/<target>/`, which the mobile builds point
// `SHERPA_ONNX_LIB_DIR` at so the `local-speech` feature links on Android and
// iOS.
//
// # Why this script exists at all
//
// `sherpa-onnx-sys` downloads its own prebuilt archive, so desktop needs
// nothing here. But its target table covers linux, macos and windows only, and
// it PANICS on anything else ("Unsupported target for sherpa-onnx prebuilt
// libs"). That table is the whole reason mobile shipped without local speech:
// Stage 3 took sherpa as a crate dependency and the crate's table quietly
// decided the mobile scope.
//
// Upstream publishes mobile artifacts at the exact same version. The crate's
// `SHERPA_ONNX_LIB_DIR` short-circuits both the download AND the target match
// (`build.rs:100-110`), so pointing it at an extracted directory is all it takes.
//
// # Pinned by CONTENT, not by tag
//
// llama.cpp is pinned by commit SHA because it is a git clone. These are release
// tarballs, so the pin is a sha256 of the archive itself, verified on every
// fetch. A release asset can in principle be replaced in place; the digest is
// what makes that loud rather than silent, which matters more here than for a
// source tree because these are BINARIES that end up inside a signed app.
//
// The version is pinned to `=1.13.3` in the crate dependency as well, so the
// headers the crate binds and the libraries staged here are one version. Bumping
// is a ritual: change VERSION, re-run with --force, record the new digests, and
// rebuild both mobile triples.
//
// # This is a build-time fetch, not a product fetch
//
// The repo's absolute rule is that the INSTALLED, RUNNING product makes no
// non-consented network request. This runs on a developer or CI machine, before
// any artifact exists, and nothing it produces fetches anything at runtime: the
// libraries end up inside the APK/IPA. Same standing as `vendor-llama.ts` and as
// the crate's own download that it replaces.
//
//   deno run -A scripts/vendor-sherpa.ts           verify, fetch if absent
//   deno run -A scripts/vendor-sherpa.ts --check   verify only, never write
//   deno run -A scripts/vendor-sherpa.ts --force   re-fetch from scratch

import { dirname, fromFileUrl, join } from "@std/path";
import { encodeHex } from "@std/encoding/hex";

/** Keep in sync with the `sherpa-onnx` pin in packages/loca-core-rs/Cargo.toml. */
const VERSION = "1.13.3";
const RELEASE_BASE = `https://github.com/k2-fsa/sherpa-onnx/releases/download/v${VERSION}`;

/** One archive per platform, with the sha256 of the exact bytes we verified. */
interface Artifact {
  /** Directory under `vendor/sherpa-onnx/`, and what the build points at. */
  readonly target: string;
  readonly archive: string;
  readonly sha256: string;
  /** Files that MUST exist under the staged dir once extraction is done. */
  readonly expect: readonly string[];
}

const ARTIFACTS: readonly Artifact[] = [
  {
    // The plain android archive, NOT `-static-link-onnxruntime`. That variant
    // ships only `libsherpa-onnx-jni.so`, the JNI wrapper, which is the wrong
    // API surface: this crate binds the C API.
    target: "android",
    archive: `sherpa-onnx-v${VERSION}-android.tar.bz2`,
    sha256: "04acacdb5eaa41d814e7792e7906168d147af6023faa9e9caf38b2f321b4cf52",
    // Exactly the pair the crate's `shared` mode emits
    // (`-l dylib=sherpa-onnx-c-api`, `-l dylib=onnxruntime`), per ABI. Only
    // arm64 is asserted because it is the only one we ship; the archive carries
    // four and the others are extracted but unused.
    expect: ["jniLibs/arm64-v8a/libsherpa-onnx-c-api.so", "jniLibs/arm64-v8a/libonnxruntime.so"],
  },
  {
    target: "ios",
    archive: `sherpa-onnx-v${VERSION}-ios.tar.bz2`,
    sha256: "2f30ff72a2f381ca39b56e62aa87b1cbdc38f19f9e49bbd92c7cc112e3a44f6a",
    // xcframeworks, not flat archives. Both halves are genuinely static (the
    // `onnxruntime.framework/onnxruntime` file is an `ar` archive, confirmed
    // with `file`), which is what makes iOS linkable at all. `stageIos` below
    // turns this layout into the names the crate emits.
    expect: [
      "build-ios/sherpa-onnx.xcframework/ios-arm64/libsherpa-onnx.a",
      "build-ios/ios-onnxruntime/1.26.0/onnxruntime.xcframework/ios-arm64/" +
        "onnxruntime.framework/onnxruntime",
    ],
  },
];

/** Where the iOS static-mode link directives are satisfied from. */
const IOS_LIB_DIR = "lib-arm64";

/** The names `sherpa-onnx-sys` emits as `-l static=<name>` (build.rs:14-29).
 *
 *  Keep in sync with the crate; a name added upstream and not listed here is a
 *  link error naming the missing library, which is the good direction. */
const IOS_STATIC_LIBS = [
  "sherpa-onnx-c-api",
  "sherpa-onnx-core",
  "kaldi-decoder-core",
  "sherpa-onnx-kaldifst-core",
  "sherpa-onnx-fstfar",
  "sherpa-onnx-fst",
  "kaldi-native-fbank-core",
  "kissfft-float",
  "piper_phonemize",
  "espeak-ng",
  "ucd",
  "onnxruntime",
  "ssentencepiece_core",
] as const;

const repoRoot = dirname(dirname(fromFileUrl(import.meta.url)));
const vendorDir = join(repoRoot, "vendor", "sherpa-onnx");

async function exists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch {
    return false;
  }
}

async function sha256(path: string): Promise<string> {
  const bytes = await Deno.readFile(path);
  return encodeHex(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)));
}

async function run(cmd: string, args: string[], cwd: string): Promise<void> {
  const { code, stderr } = await new Deno.Command(cmd, {
    args,
    cwd,
    stdout: "null",
    stderr: "piped",
  }).output();
  if (code !== 0) {
    throw new Error(`${cmd} ${args.join(" ")} failed:\n${new TextDecoder().decode(stderr)}`);
  }
}

/** Fetch, verify the digest, and extract. The digest is checked BEFORE the
 *  archive is unpacked, so bytes that are not the pinned ones never reach the
 *  filesystem as files. */
async function fetchAndStage(artifact: Artifact): Promise<void> {
  const dest = join(vendorDir, artifact.target);
  await Deno.mkdir(dest, { recursive: true });
  const archivePath = join(dest, artifact.archive);

  if (!(await exists(archivePath))) {
    const url = `${RELEASE_BASE}/${artifact.archive}`;
    console.log(`fetching ${artifact.archive}`);
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`GET ${url} answered ${response.status}`);
    }
    await Deno.writeFile(archivePath, response.body!);
  }

  const digest = await sha256(archivePath);
  if (digest !== artifact.sha256) {
    // Removed, so a re-run refetches rather than re-checking the same bad file
    // forever.
    await Deno.remove(archivePath).catch(() => {});
    throw new Error(
      `${artifact.archive} has sha256 ${digest}, expected ${artifact.sha256}. ` +
        `The pinned release asset does not match what upstream is serving.`,
    );
  }

  await run("tar", ["xjf", artifact.archive], dest);

  if (artifact.target === "ios") {
    await stageIos(dest);
  }
}

/** Build the lib dir the crate's iOS static link expects.
 *
 *  The mismatch this resolves: the crate emits THIRTEEN `-l static=<name>`
 *  directives, and upstream's iOS build ships ONE merged archive containing all
 *  the sherpa components, plus onnxruntime inside a framework wrapper. So:
 *
 *  - the merged archive answers `sherpa-onnx-c-api` (it carries every sherpa
 *    symbol, so which of the thirteen names it takes is arbitrary),
 *  - the framework's binary, renamed, answers `onnxruntime`,
 *  - the remaining eleven get EMPTY `ar` archives, which link cleanly and
 *    contribute nothing.
 *
 *  Unpretty and entirely standard. The alternative is forking
 *  `sherpa-onnx-sys` so its mobile arm emits directives matching what upstream
 *  ships: cleaner to read, one more third-party crate to own and to re-fork on
 *  every bump. Not worth it for eleven empty files. */
async function stageIos(dest: string): Promise<void> {
  const libDir = join(dest, IOS_LIB_DIR);
  await Deno.mkdir(libDir, { recursive: true });

  await thinToArm64(
    join(dest, "build-ios/sherpa-onnx.xcframework/ios-arm64/libsherpa-onnx.a"),
    join(libDir, "libsherpa-onnx-c-api.a"),
  );
  await thinToArm64(
    join(
      dest,
      "build-ios/ios-onnxruntime/1.26.0/onnxruntime.xcframework/ios-arm64/" +
        "onnxruntime.framework/onnxruntime",
    ),
    join(libDir, "libonnxruntime.a"),
  );

  const filled = new Set(["sherpa-onnx-c-api", "onnxruntime"]);
  for (const name of IOS_STATIC_LIBS) {
    if (filled.has(name)) continue;
    const path = join(libDir, `lib${name}.a`);
    if (await exists(path)) continue;
    // The eight-byte archive magic and nothing else, which IS a valid archive
    // with zero members. Written directly rather than shelled out to `ar`,
    // because macOS `ar` refuses to create an archive with no input files at
    // all: `ar -crs foo.a` is a usage error, not an empty archive. Every linker
    // that reads `.a` files accepts this header; it contributes no symbols,
    // which is exactly the point.
    await Deno.writeFile(path, new TextEncoder().encode("!<arch>\n"));
  }
}

/** Copy `src` to `dst`, extracting the arm64 slice if it is a fat binary.
 *
 *  Needed because upstream's `onnxruntime.framework/onnxruntime` is a Mach-O
 *  UNIVERSAL archive wrapping a single-arch `ar` archive (`cafebabe` magic, one
 *  slice). rustc's `-l static=` reads the file as a plain archive and refuses it
 *  with "Unsupported archive identifier", so a straight copy produces a link
 *  error that names the file and not the reason. `file` reporting it as an "ar
 *  archive" is what made this easy to miss: that is what it says about the SLICE.
 *
 *  Applied to both inputs rather than only the one known to be fat, so a future
 *  upstream that fattens the other is handled rather than rediscovered. */
async function thinToArm64(src: string, dst: string): Promise<void> {
  const { code } = await new Deno.Command("lipo", {
    args: [src, "-thin", "arm64", "-output", dst],
    stdout: "null",
    stderr: "null",
  }).output();
  // `lipo -thin` fails on an input that is already thin ("must be a fat file"),
  // which is the ordinary case for the merged sherpa archive.
  if (code !== 0) {
    await Deno.copyFile(src, dst);
  }
}

/** Everything the artifact promised is on disk. */
async function staged(artifact: Artifact): Promise<boolean> {
  const dest = join(vendorDir, artifact.target);
  for (const rel of artifact.expect) {
    if (!(await exists(join(dest, rel)))) return false;
  }
  if (artifact.target === "ios") {
    // The staged lib dir is the thing the build actually points at, so its
    // absence means not-staged even when the archive extracted fine.
    for (const name of IOS_STATIC_LIBS) {
      if (!(await exists(join(dest, IOS_LIB_DIR, `lib${name}.a`)))) return false;
    }
  }
  return true;
}

async function main(): Promise<void> {
  const check = Deno.args.includes("--check");
  const force = Deno.args.includes("--force");

  if (check && force) {
    console.error("--check and --force are mutually exclusive");
    Deno.exit(2);
  }
  if (force) {
    await Deno.remove(vendorDir, { recursive: true }).catch(() => {});
  }

  for (const artifact of ARTIFACTS) {
    if (await staged(artifact)) {
      console.log(`ok sherpa-onnx ${VERSION} ${artifact.target}`);
      continue;
    }
    if (check) {
      console.error(
        `vendor/sherpa-onnx/${artifact.target} is missing or incomplete.\n` +
          `Run: deno run -A scripts/vendor-sherpa.ts`,
      );
      Deno.exit(1);
    }
    await fetchAndStage(artifact);
    if (!(await staged(artifact))) {
      console.error(
        `${artifact.archive} extracted but did not contain ${artifact.expect.join(", ")}. ` +
          `The archive layout changed; this script needs updating alongside the version pin.`,
      );
      Deno.exit(1);
    }
    console.log(`ok sherpa-onnx ${VERSION} ${artifact.target}`);
  }
}

await main();
