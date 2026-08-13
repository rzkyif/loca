// Install phase: run `deno install` for an already-downloaded extension's declared
// dependencies, pin its content hash, and flip the row to status='installed'.
// `deno install --node-modules-dir=auto` lands node_modules + deno.lock in the
// extension folder (both hash-excluded), so the folder stays under ~/.loca/
// without us editing the shipped deno.json.
import { join } from "@std/path";
import { paths } from "../paths.ts";
import { AppError } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { requireWorkerDeno } from "../sidecars/worker-deno.ts";
import { hashExtension } from "./hash.ts";
import { extensionsRegistry } from "./registry.ts";
import { type InstallEventSink, readOptional } from "./installer-shared.ts";

const log = getLogger("extension-installer");

export async function runInstallDeps(
  extensionId: string,
  jobId: string,
  sink: InstallEventSink,
): Promise<void> {
  const registry = extensionsRegistry();
  const tk = registry.get(extensionId);
  if (!tk) {
    throw new AppError("extension_not_found", `extension ${extensionId} not found`);
  }
  if (tk.status === "drift") {
    throw new AppError(
      "extension_hash_drift",
      `extension ${extensionId} has drifted; confirm re-enable before installing`,
    );
  }
  // Install deps in place: the download already swapped the folder into its final
  // location, no worker can spawn against a not-yet-installed extension (so no
  // race), and node_modules + deno.lock are hash-excluded. No second swap.
  await installDeps(tk.installedPath, jobId, extensionId, sink);
  // Pin the content hash AFTER deps land. node_modules + deno.lock are excluded
  // from the hash, so this equals the pristine downloaded content.
  const contentHash = await hashExtension(tk.installedPath);
  registry.markInstalled(extensionId, contentHash);
  log.info(`installed ${extensionId}`);
}

/** Install a extension's declared dependencies with `deno install`, when it
 *  declares any. `--node-modules-dir=auto` lands node_modules in the folder
 *  without us editing the shipped deno.json. Run from the install phase on the
 *  live folder. */
async function installDeps(
  dir: string,
  jobId: string,
  extensionId: string,
  sink: InstallEventSink,
): Promise<void> {
  if (await hasDeclaredDeps(dir)) {
    await runDenoInstall(dir, jobId, extensionId, sink);
  }
}

/** True when the extension declares dependencies in deno.json `imports`
 *  (npm:/jsr: specifiers) or package.json `dependencies`. Throws on an
 *  unparseable config rather than silently skipping its deps. */
export async function hasDeclaredDeps(dir: string): Promise<boolean> {
  const denoText = await readOptional(join(dir, "deno.json"));
  if (denoText) {
    let cfg: { imports?: Record<string, string> };
    try {
      cfg = JSON.parse(denoText);
    } catch {
      throw new AppError("deps_install_failed", `unparseable deno.json in ${dir}`);
    }
    for (const spec of Object.values(cfg.imports ?? {})) {
      if (typeof spec === "string" && (spec.startsWith("npm:") || spec.startsWith("jsr:"))) {
        return true;
      }
    }
  }
  const pkgText = await readOptional(join(dir, "package.json"));
  if (pkgText) {
    let pkg: { dependencies?: Record<string, string> };
    try {
      pkg = JSON.parse(pkgText);
    } catch {
      throw new AppError("deps_install_failed", `unparseable package.json in ${dir}`);
    }
    if (pkg.dependencies && Object.keys(pkg.dependencies).length > 0) {
      return true;
    }
  }
  return false;
}

async function runDenoInstall(
  cwd: string,
  jobId: string,
  extensionId: string,
  sink: InstallEventSink,
): Promise<void> {
  // Resolve the deno worker runtime, surfacing a clean "download required files
  // from Settings" error if it isn't installed yet (instead of a raw spawn
  // ENOENT). Only extensions that declare deps reach here.
  const denoBin = await requireWorkerDeno();
  // --node-modules-dir=auto lands node_modules in the extension folder without us
  // editing the shipped deno.json; DENO_DIR keeps the package cache under
  // ~/.loca. We omit --allow-scripts so npm lifecycle scripts are denied by
  // default (deno rejects the flag when its value isn't an npm: specifier).
  const proc = new Deno.Command(denoBin, {
    args: ["install", "--node-modules-dir=auto", "--quiet"],
    cwd,
    env: { DENO_DIR: paths().denoCacheDir },
    stdout: "piped",
    stderr: "piped",
  }).spawn();

  // Mirror stderr into the thrown error so a failed install is debuggable from
  // core.log too (the client also receives each line via install_log frames).
  const stderr: string[] = [];
  await Promise.all([
    pumpLines(proc.stdout, (line) => sink.log(jobId, extensionId, "stdout", line)),
    pumpLines(proc.stderr, (line) => {
      stderr.push(line);
      sink.log(jobId, extensionId, "stderr", line);
    }),
  ]);
  const status = await proc.status;
  if (!status.success) {
    const tail = stderr
      .filter((l) => l.trim())
      .slice(-12)
      .join("\n");
    throw new AppError(
      "deps_install_failed",
      `deno install exited ${status.code} for ${extensionId}${tail ? `:\n${tail}` : ""}`,
    );
  }
}

async function pumpLines(
  stream: ReadableStream<Uint8Array>,
  fn: (line: string) => void,
): Promise<void> {
  const decoder = new TextDecoder();
  let buf = "";
  for await (const chunk of stream) {
    buf += decoder.decode(chunk, { stream: true });
    const lines = buf.split("\n");
    buf = lines.pop() ?? "";
    for (const line of lines) {
      if (line.length > 0) fn(line);
    }
  }
  if (buf.length > 0) fn(buf);
}
