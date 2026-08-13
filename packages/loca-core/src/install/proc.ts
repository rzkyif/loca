// Subprocess helpers for the install subcommands. Kept tiny and dependency-free
// (no engine host, no logger): these run before the server boots.

import { progress } from "./io.ts";

export interface RunResult {
  code: number;
  success: boolean;
  stdout: string;
  stderr: string;
}

/** Run a command to completion, capturing output. On a non-zero exit it prints
 *  the stderr as progress unless `ignoreError` is set (used for probes and
 *  best-effort teardown steps that are non-zero for benign reasons).
 *
 *  `capture: false` runs the child with null stdio instead of pipes. Required
 *  when the child (or a grandchild) launches a process that outlives it: on
 *  Windows a detached grandchild inherits the pipe write ends (.NET's
 *  Start-Process passes bInheritHandles), so reading the pipes to EOF would
 *  block for as long as that process lives - the "stuck on installing" hang. */
export async function run(
  argv: string[],
  opts: { env?: Record<string, string>; ignoreError?: boolean; capture?: boolean } = {},
): Promise<RunResult> {
  const capture = opts.capture ?? true;
  const cmd = new Deno.Command(argv[0], {
    args: argv.slice(1),
    env: opts.env,
    stdin: "null",
    stdout: capture ? "piped" : "null",
    stderr: capture ? "piped" : "null",
  });
  let out: Deno.CommandOutput;
  try {
    out = await cmd.output();
  } catch (err) {
    if (!opts.ignoreError) {
      progress(`failed to spawn ${argv[0]}: ${err instanceof Error ? err.message : String(err)}`);
    }
    return { code: 127, success: false, stdout: "", stderr: String(err) };
  }
  const dec = new TextDecoder();
  // Deno.CommandOutput throws on reading a stream that wasn't piped.
  const res: RunResult = {
    code: out.code,
    success: out.success,
    stdout: capture ? dec.decode(out.stdout) : "",
    stderr: capture ? dec.decode(out.stderr) : "",
  };
  if (!res.success && !opts.ignoreError) {
    progress(`command failed (exit ${res.code}): ${argv.join(" ")}: ${res.stderr.trim()}`);
  }
  return res;
}

/** Run a PowerShell snippet (Windows service registration / ACLs). Returns the
 *  result; callers decide whether a non-zero exit is fatal. */
export async function runPwsh(
  script: string,
  opts: { ignoreError?: boolean; capture?: boolean } = {},
): Promise<RunResult> {
  return await run(
    [
      "powershell",
      "-NoProfile",
      "-NonInteractive",
      "-ExecutionPolicy",
      "Bypass",
      "-Command",
      script,
    ],
    opts,
  );
}

/** The current user's home directory, independent of the LOCA_CORE_HOME test
 *  override (service files must land in the real ~/Library or ~/.config even
 *  when core state is redirected to a tempdir). */
export function realHome(): string {
  const home = Deno.env.get("HOME") ?? Deno.env.get("USERPROFILE");
  if (!home) throw new Error("could not determine home directory (no HOME or USERPROFILE)");
  return home;
}
