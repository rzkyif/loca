// Fetches the pinned llama.cpp source into `vendor/llama.cpp`, which the Rust
// core's `build.rs` compiles when the `local-inference` feature is on.
//
// The pin is a SHA, not a tag: a tag can be moved, and the whole point is that
// the tree we build is the tree the S0.2 spike was verified against (D-002).
// The tag is recorded alongside it only so a human can find the release notes.
//
// The SHA is written to `vendor/.llama-pin`, which `build.rs` reads and
// compares against its own constant. The two must agree or the build fails, so
// a hand-edited or half-updated vendor tree cannot silently produce a binary
// built from a different llama.cpp than the one recorded.
//
// `vendor/` is gitignored. That keeps a multi-hundred-MB third-party tree out of
// the repo, and it means a release is only as reproducible as this pin; the
// standing plan is to convert it to a submodule.
//
//   deno run -A scripts/vendor-llama.ts           verify, fetch if absent
//   deno run -A scripts/vendor-llama.ts --check   verify only, never write
//   deno run -A scripts/vendor-llama.ts --force   re-fetch from scratch

import { dirname, fromFileUrl, join } from "@std/path";

/** Keep in sync with `LLAMA_CPP_PIN` in packages/loca-core-rs/build.rs. */
const PIN = "1d2869c6e54d5003f3927a79efbca0fefa034a6d";
/** The upstream release tag that points at {@link PIN}. Informational. */
const TAG = "b10415";
const REPO = "https://github.com/ggml-org/llama.cpp";

const repoRoot = dirname(dirname(fromFileUrl(import.meta.url)));
const vendorDir = join(repoRoot, "vendor");
const srcDir = join(vendorDir, "llama.cpp");
const pinFile = join(vendorDir, ".llama-pin");

async function run(cmd: string, args: string[], cwd: string): Promise<string> {
  const { code, stdout, stderr } = await new Deno.Command(cmd, {
    args,
    cwd,
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (code !== 0) {
    throw new Error(`${cmd} ${args.join(" ")} failed:\n${new TextDecoder().decode(stderr)}`);
  }
  return new TextDecoder().decode(stdout).trim();
}

async function exists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch {
    return false;
  }
}

/** The SHA actually checked out in the vendored tree, or null if it is not a
 *  readable clone. Read from the vendored tree itself rather than from the pin
 *  file, so a stale pin file is caught rather than trusted. */
async function vendoredSha(): Promise<string | null> {
  if (!(await exists(join(srcDir, ".git")))) return null;
  try {
    return await run("git", ["rev-parse", "HEAD"], srcDir);
  } catch {
    return null;
  }
}

async function fetchPinned(): Promise<void> {
  await Deno.mkdir(vendorDir, { recursive: true });
  console.log(`vendoring llama.cpp ${TAG} (${PIN.slice(0, 12)}) into vendor/llama.cpp`);
  // Blobless rather than shallow: a --depth 1 clone of a tag cannot be
  // rev-parsed back to an arbitrary pinned SHA if the tag ever moves, and the
  // whole verification below depends on being able to check out the SHA itself.
  await run("git", ["clone", "--filter=blob:none", "--no-checkout", REPO, srcDir], vendorDir);
  await run("git", ["checkout", "--detach", PIN], srcDir);
}

async function main(): Promise<void> {
  const check = Deno.args.includes("--check");
  const force = Deno.args.includes("--force");

  if (force) {
    if (check) {
      console.error("--check and --force are mutually exclusive");
      Deno.exit(2);
    }
    await Deno.remove(srcDir, { recursive: true }).catch(() => {});
  }

  let sha = await vendoredSha();

  if (sha === null) {
    if (check) {
      console.error(
        `vendor/llama.cpp is missing or is not a git clone.\n` +
          `Run: deno run -A scripts/vendor-llama.ts`,
      );
      Deno.exit(1);
    }
    await fetchPinned();
    sha = await vendoredSha();
  }

  if (sha !== PIN) {
    console.error(
      `vendor/llama.cpp is at ${sha}, expected ${PIN} (${TAG}).\n` +
        `Re-fetch with: deno run -A scripts/vendor-llama.ts --force`,
    );
    Deno.exit(1);
  }

  if (!check) await Deno.writeTextFile(pinFile, `${PIN}\n`);
  console.log(`ok llama.cpp ${TAG} ${PIN.slice(0, 12)}`);
}

await main();
