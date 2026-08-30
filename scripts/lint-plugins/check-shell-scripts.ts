// Runs shellcheck over every tracked shell script. The installer templates
// under scripts/release/templates/ ship inside the native installers (the macOS
// uninstaller runs as root on user machines), so a shell bug there is a
// security-relevant bug; oxlint never sees .sh, so nothing else guards them.
//
// Wired into `deno task lint`. Scans `--severity=warning` and above to focus on
// real problems over style nits.
//
// shellcheck is an external binary; when it is not installed this walker SKIPS
// with a visible warning and exits 0, so a contributor without it can still run
// `deno task lint` (CI installs it, so the check is enforced there). PowerShell
// (.ps1) is not covered: shellcheck cannot parse it.

import { fromFileUrl } from "@std/path";

// Native OS path (fromFileUrl); URL .pathname is an invalid "/C:/..." cwd on Windows.
const ROOT = fromFileUrl(new URL("../../", import.meta.url));
const SEVERITY = "warning";

async function trackedShellScripts(): Promise<string[]> {
  // --cached + --others (minus ignored): tracked files AND new not-yet-added
  // ones, so a fresh script is linted before its first commit.
  const out = await new Deno.Command("git", {
    args: ["ls-files", "-z", "--cached", "--others", "--exclude-standard", "*.sh"],
    cwd: ROOT,
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (!out.success) {
    console.error("check-shell-scripts: `git ls-files` failed.");
    console.error(new TextDecoder().decode(out.stderr));
    Deno.exit(2);
  }
  const tracked = new TextDecoder()
    .decode(out.stdout)
    .split("\0")
    .filter((p) => p.length > 0);
  // ls-files lists a deleted-but-unstaged file too; skip anything not on disk
  // so a pending deletion doesn't fail the walk.
  const present: string[] = [];
  for (const p of tracked) {
    try {
      await Deno.stat(new URL(p, new URL("../../", import.meta.url)));
      present.push(p);
    } catch {
      // deleted in the working tree
    }
  }
  return present;
}

async function shellcheckAvailable(): Promise<boolean> {
  try {
    const { success } = await new Deno.Command("shellcheck", {
      args: ["--version"],
      stdout: "null",
      stderr: "null",
    }).output();
    return success;
  } catch {
    return false; // not on PATH
  }
}

const files = await trackedShellScripts();
if (files.length === 0) {
  console.log("check-shell-scripts: no tracked .sh files.");
  Deno.exit(0);
}

if (!(await shellcheckAvailable())) {
  console.warn(
    "check-shell-scripts: shellcheck not found on PATH; SKIPPING.\n" +
      "  Install it to lint the shell scripts locally:\n" +
      "    macOS:  brew install shellcheck\n" +
      "    Debian: apt install shellcheck\n" +
      "  CI installs it, so this check is still enforced there.",
  );
  Deno.exit(0);
}

const { code, stdout, stderr } = await new Deno.Command("shellcheck", {
  args: [`--severity=${SEVERITY}`, "--format=gcc", "--", ...files],
  cwd: ROOT,
  stdout: "piped",
  stderr: "piped",
}).output();

const out = (new TextDecoder().decode(stdout) + new TextDecoder().decode(stderr)).trimEnd();
if (code !== 0) {
  console.error(`shellcheck reported issues (severity >= ${SEVERITY}):\n`);
  if (out) console.error(out);
  console.error(
    "\nFix each finding, or annotate a justified exception inline with a\n" +
      "`# shellcheck disable=SCXXXX` directive above the line (with a reason).",
  );
  Deno.exit(1);
}
