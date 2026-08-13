import { execFileSync } from "node:child_process";

/** Last-commit date (YYYY-MM-DD) for a repo file, used as the manual's
 *  "Last updated" stamp. `git log -1 --format=%cs` already emits the ISO short
 *  date we render. Files with no commit yet (uncommitted/untracked) fall back to
 *  the build date. Runs at build time, where `astro build`'s cwd is the website
 *  dir and `filePath` is relative to it. */
export function gitLastUpdated(filePath: string | undefined): string {
  try {
    if (!filePath) throw new Error("no file path");
    const out = execFileSync("git", ["log", "-1", "--format=%cs", "--", filePath], {
      encoding: "utf8",
    }).trim();
    if (out) return out;
  } catch {
    // git missing or the file has no history; fall through to the build date.
  }
  return new Date().toISOString().slice(0, 10);
}
