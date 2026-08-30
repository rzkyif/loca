// Runs a command with the mobile native-library environment set, then execs it.
//
//   deno run -A scripts/mobile-cargo.ts <triple> -- <command> [args...]
//
// # What it sets, and why it cannot be a config file
//
// `sherpa-onnx-sys` resolves its libraries from `SHERPA_ONNX_LIB_DIR` and
// nothing else, and cargo's `[env]` table has no per-target form, so there is no
// declarative place to say "this directory for android, that one for ios".
//
// It also verifies the vendored tree is staged before handing off, so a missing
// `vendor/sherpa-onnx` fails here with the command that fixes it.
//
// # This is for single-target invocations only
//
// `SHERPA_ONNX_LIB_DIR` is ONE value, and an Android release builds four ABIs.
// This wrapper takes one triple, so it serves `check:android` / `check:ios` and
// a single-target build, and it is the WRONG place to fix a multi-ABI build: all
// four gradle rust tasks would inherit the one value it exports.
//
// The multi-ABI case is handled where it can be: `BuildTask.kt` in the android
// gradle `buildSrc` sets the variable per task from that task's own target, so
// each ABI gets its own directory. See the comment there.

import { dirname, fromFileUrl, join } from "@std/path";

const repoRoot = dirname(dirname(fromFileUrl(import.meta.url)));

/** Android ABI directory names, keyed by rust target triple. */
const ANDROID_ABI: Record<string, string> = {
  "aarch64-linux-android": "arm64-v8a",
  "armv7-linux-androideabi": "armeabi-v7a",
  "i686-linux-android": "x86",
  "x86_64-linux-android": "x86_64",
};

function libDirFor(triple: string): string | null {
  const abi = ANDROID_ABI[triple];
  if (abi) {
    return join(repoRoot, "vendor", "sherpa-onnx", "android", "jniLibs", abi);
  }
  if (triple.endsWith("-apple-ios")) {
    // The staged directory `vendor-sherpa.ts` builds, not the xcframework:
    // upstream ships one merged archive plus onnxruntime, and the crate emits
    // thirteen `-l static=` directives, so the names have to be arranged first.
    return join(repoRoot, "vendor", "sherpa-onnx", "ios", "lib-arm64");
  }
  return null;
}

async function main(): Promise<void> {
  const separator = Deno.args.indexOf("--");
  if (separator < 1 || separator === Deno.args.length - 1) {
    console.error("usage: deno run -A scripts/mobile-cargo.ts <triple> -- <command> [args...]");
    Deno.exit(2);
  }
  const triple = Deno.args[0];
  const command = Deno.args.slice(separator + 1);

  const env: Record<string, string> = {};
  const libDir = libDirFor(triple);
  if (libDir === null) {
    console.error(
      `${triple} is not a mobile target this wrapper knows. Known: ` +
        `${Object.keys(ANDROID_ABI).join(", ")}, *-apple-ios.`,
    );
    Deno.exit(2);
  }
  try {
    const stat = await Deno.stat(libDir);
    if (!stat.isDirectory) throw new Error("not a directory");
  } catch {
    console.error(
      `vendor/sherpa-onnx is not staged for ${triple} (looked in ${libDir}).\n` +
        `Run: deno run -A scripts/vendor-sherpa.ts`,
    );
    Deno.exit(1);
  }
  env.SHERPA_ONNX_LIB_DIR = libDir;

  const { code } = await new Deno.Command(command[0], {
    args: command.slice(1),
    env,
    stdout: "inherit",
    stderr: "inherit",
  }).output();
  Deno.exit(code);
}

await main();
