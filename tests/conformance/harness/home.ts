// Seeds a throwaway LOCA_CORE_HOME before a core boots.
//
// Target-aware on purpose. The Deno core's boot check refuses to start without
// four native helper binaries in its bin dir; the Rust core absorbs all but the
// updater (D-000), so the required set differs. Deriving it from the target
// keeps that difference in one place instead of spreading it through cases.

import { join } from "@std/path";
import { REQUIRED_HELPERS } from "./repo.ts";

export type Target = "deno" | "rust-desktop" | "rust-mobile";

/** The fixed admin token every scenario seeds. Writing it into a home we just
 *  created is the installer's own privilege (install/bootstrap.ts:38 mints this
 *  file), not an auth bypass: the core still verifies it. */
export const ADMIN_TOKEN = "conformance-admin-token";

export interface SeedOptions {
  target: Target;
  upstreamBaseUrl: string;
  /** Extra settings.json entries, merged over the mock-wired defaults. */
  settings?: Record<string, unknown>;
  /** "stub" (default) writes empty files to satisfy the boot-time presence
   *  check, which stats them and never runs them. The dev channel also never
   *  uses the OS keychain, so nothing executes a helper on the paths the
   *  foundation slice covers. A case that genuinely invokes a helper asks for
   *  "real" and gets an actionable error when the build is absent.
   *
   *  INVARIANT, and it is worth defending: the DEFAULT boot path needs nothing
   *  built. A suite that requires `deno task dev` first is a suite people skip;
   *  one that runs on a clean checkout runs in CI and inside a bisect. If a
   *  future case needs a built artifact, it opts in HERE, per case. Never move
   *  a build prerequisite into the default path to make one case simpler. */
  helpers?: "stub" | "real";
  /** Files to write into the home before any core boots.
   *
   *  For the cutover cases whose fixture no HTTP step can build. A model
   *  "downloaded by the Deno core" is a multi-gigabyte transfer the suite
   *  cannot perform and must not attempt, and the property under test is not
   *  the download: it is whether both cores agree that the resulting FILE is
   *  present. Planting the end state tests exactly that and nothing else.
   *
   *  Paths are relative to the home and may contain directories, which are
   *  created. `LOCA_CORE_HOME` roots `models/` inside the home
   *  (`paths.rs:69-76`), so a planted model cannot reach the developer's real
   *  `~/.loca/models`. That containment is what makes this safe to use at all;
   *  keep it in mind before planting anything outside the home. */
  plant?: PlantedFile[];
}

export interface PlantedFile {
  /** Relative to the home. Parent directories are created. */
  path: string;
  /** Literal text. Mutually exclusive with `bytes`. */
  content?: string;
  /** Write this many zero bytes instead, for a file whose SIZE is the point
   *  and whose contents are not. */
  bytes?: number;
}

function defaultSettings(upstream: string): Record<string, unknown> {
  return {
    // External providers everywhere, so no local inference or speech sidecar
    // is spawned and no weights are needed.
    "llm.provider": "external",
    "llm.external.baseUrl": `${upstream}/v1`,
    "llm.external.model": "mock-model",
    "stt.provider": "external",
    "stt.external.baseUrl": `${upstream}/v1`,
    "stt.external.model": "whisper-1",
    "tts.enabled": false,
    "tts.external.baseUrl": `${upstream}/v1`,
    "tts.external.model": "tts-1",
    "dualModel.external.baseUrl": `${upstream}/v1`,
    "dualModel.external.model": "secondary-model",
  };
}

function helperNames(target: Target): string[] {
  // The Rust core absorbs every helper except the updater (D-000).
  return target === "deno" ? REQUIRED_HELPERS : ["loca-core-updater"];
}

async function writeStub(path: string): Promise<void> {
  await Deno.writeTextFile(path, "");
  if (Deno.build.os !== "windows") await Deno.chmod(path, 0o755);
}

async function linkReal(binDir: string, base: string, suffixed: string): Promise<void> {
  const { REPO_ROOT } = await import("./repo.ts");
  const src = join(REPO_ROOT, "target", "debug", base);
  try {
    await Deno.stat(src);
  } catch {
    throw new Error(
      `this case needs the real ${base} binary, which is not built. Run ` +
        `\`cargo build -p ${base}\` (or \`deno task dev\` once) and retry.`,
    );
  }
  await Deno.symlink(src, join(binDir, suffixed));
}

/** Create and seed a throwaway core home. Returns its path. */
export async function seedHome(opts: SeedOptions): Promise<string> {
  const home = await Deno.makeTempDir({ prefix: "loca-conformance-" });
  const binDir = join(home, "bin");
  await Deno.mkdir(binDir, { recursive: true });

  // Channel-suffixed names, matching binaries/versions.ts on the dev channel.
  for (const base of helperNames(opts.target)) {
    const suffixed = `${base}-dev`;
    if ((opts.helpers ?? "stub") === "real") await linkReal(binDir, base, suffixed);
    else await writeStub(join(binDir, suffixed));
  }

  const settings = { ...defaultSettings(opts.upstreamBaseUrl), ...opts.settings };
  await Deno.writeTextFile(join(home, "settings.json"), JSON.stringify(settings, null, 2));
  await Deno.writeTextFile(join(home, ".admin-token"), `${ADMIN_TOKEN}\n`);

  for (const file of opts.plant ?? []) {
    if (file.path.includes("..")) {
      throw new Error(`planted path escapes the home: ${file.path}`);
    }
    const target = join(home, file.path);
    await Deno.mkdir(join(target, ".."), { recursive: true });
    if (file.bytes !== undefined) await Deno.writeFile(target, new Uint8Array(file.bytes));
    else await Deno.writeTextFile(target, file.content ?? "");
  }
  return home;
}
